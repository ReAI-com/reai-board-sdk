//! Consumer 通道的「当前按着哪些键」账本。
//!
//! ## 为什么需要它
//!
//! 本设备的 Consumer 通道（USB 的 0x000C 接口、BLE 的 0x0C 事件）一次只能报**一个**键值。
//! 按住 Tab 再转旋钮，报文会被旋钮的键值顶掉——但那不代表 Tab 松开了。
//!
//! 两条链路原先各自用「最后收到的那个键」当作当前状态，于是同一个 bug 各犯一次：
//! 转旋钮的瞬间按住的键被判成松开，按住不放的效果（比如按住 Tab 打开的应用切换器）
//! 当场消失；BLE 那边更糟——释放事件被记到旋钮头上，按住的那个键**再也等不到松手**，
//! 只能靠上层 30 秒硬超时兜底。
//!
//! 所以这份账本放在内核层，USB 与 BLE 共用同一套解释规则，不再各写一份。
//!
//! ## 两条规则
//!
//! 1. **新键值是追加，不是替换**。两次按键之间只要有过一次 0x0000，集合就已经清空了；
//!    收不到 0x0000 就直接来了第二个键值，恰恰说明这两个键真的重叠着。
//! 2. **紧跟旋钮脉冲的 0x0000 是那一格的收尾**，不是「所有键都松开了」。
//!    旋钮自己按「敲一下」上报（进一次、出一次），按住的键全程不动。

use crate::kernel::event::KeySource;
use crate::kernel::key_aggregator::PressedKeyMeta;
use crate::kernel::protocol_hid::{find_key_index_by_value, get_key_name, is_knob_pulse_key_index};
use std::time::{Duration, Instant};

/// 旋钮脉冲与它的收尾归零帧之间，允许的最大间隔。
///
/// 这个窗口是用来回答「这一帧 0x0000 是刚才那格旋钮的收尾，还是用户松手了」的。
/// 实测（USB，固件 1.54）两者相差三个数量级：
///
/// - 脉冲键值 → 收尾归零：**约 1.4 毫秒**
/// - 最后一格旋钮 → 用户松手的归零：**几百毫秒**（抓到 565 毫秒与 1 秒）
///
/// 取 100 毫秒是实测值的约 70 倍余量，同时远小于人手松开的间隔。
/// 用时间而不是「上一帧是什么」来判，是因为中间可能插进别的帧（设备信息响应
/// 就会在 Consumer 接口上冒出未知键值），靠标记会两头都判错。
///
/// BLE 与 USB 共用这个窗口，且 BLE 也已真机验收通过。看起来蓝牙的通知延迟更高、
/// 窗口该放宽，但要判的是**脉冲与它自己的收尾帧之间**的间隔——这两帧由固件在同一次
/// 转动里连发、走同一条链路，链路慢是一起慢，间隔并不会被拉开。
/// 反过来窗口越大越危险：转完立刻松手的那一帧会被误吞成收尾，切换器就关不掉了。
const KNOB_PULSE_TAIL_WINDOW: Duration = Duration::from_millis(100);

/// 处理完一帧 Consumer 报文之后要做的事。
pub(crate) struct ConsumerFrame {
    /// 依次上报给聚合器的按键集合（空 = 这一帧不改变任何状态）。
    pub(crate) batches: Vec<Vec<PressedKeyMeta>>,
    /// 这一帧是不是「全部松开」（AI 语音键的释放判定挂在它上面）。
    pub(crate) cleared: bool,
    /// 是否计入按键事件统计。
    ///
    /// 只有 USB 侧有统计计数器会读它；BLE 侧没有对应指标，忽略即可。
    pub(crate) counts_as_key_event: bool,
}

/// Consumer 通道的「还按着哪些键」。
///
/// ## 为什么需要它
///
/// 这条通道一次只能报**一个**键值。按住 Tab 再转旋钮，报文会被旋钮的键值顶掉——
/// 但那不代表 Tab 松开了。原来的实现直接拿这一个键值当作「当前按下的全部键」，
/// 于是转旋钮的那一瞬间 Tab 就被判成松开：按住不放的效果当场消失，
/// 应用切换器被关掉，后面每转一格还会去触发旋钮自己的绑定。
///
/// ## 两条规则
///
/// 1. **新键值是追加，不是替换**。两次按键之间只要有过一次 0x0000，集合就已经清空了；
///    收不到 0x0000 就直接来了第二个键值，恰恰说明这两个键真的重叠着。
/// 2. **紧跟旋钮脉冲的 0x0000 是那一格的收尾**，不是「所有键都松开了」。
///    旋钮自己按「敲一下」上报（进一次、出一次），按住的键全程不动。
///    「紧跟」按时间判（见 [`KNOB_PULSE_TAIL_WINDOW`]），不按「上一帧是什么」——
///    中间随时可能插进别的帧。
#[derive(Default)]
pub(crate) struct ConsumerHeldTracker {
    /// 当前认为还按着的键，按按下顺序。
    held: Vec<PressedKeyMeta>,
    /// 最近一次旋钮脉冲的时刻；收尾帧吃掉之后清空，一格只配一次收尾。
    last_knob_pulse_at: Option<Instant>,
}

impl ConsumerHeldTracker {
    /// 吃进一帧原始键值，算出要上报什么。
    ///
    /// `now` 由调用方传入而不是内部读时钟，测试才能精确构造「收尾」与「松手」两种时序。
    pub(crate) fn on_frame(&mut self, key_value: u16, now: Instant) -> ConsumerFrame {
        if key_value == 0x0000 {
            let within_pulse_tail = self
                .last_knob_pulse_at
                .is_some_and(|at| now.saturating_duration_since(at) <= KNOB_PULSE_TAIL_WINDOW);
            if within_pulse_tail {
                // 刚才那一格旋钮的收尾。按住的键与它无关，一个都不能动。
                log::debug!(
                    target: "hid",
                    "Consumer 0x0000 是旋钮脉冲收尾，按住的 {:?} 保持不变",
                    self.held_indices()
                );
                // 一格旋钮只配一次收尾：紧接着再来的 0x0000 就是真松手了。
                self.last_knob_pulse_at = None;
                return ConsumerFrame {
                    batches: Vec::new(),
                    cleared: false,
                    counts_as_key_event: false,
                };
            }
            log::debug!(
                target: "hid",
                "Consumer 0x0000 全部松开（此前按住 {:?}）",
                self.held_indices()
            );
            self.held.clear();
            return ConsumerFrame {
                batches: vec![Vec::new()],
                cleared: true,
                counts_as_key_event: false,
            };
        }

        let Some(key_index) = find_key_index_by_value(key_value) else {
            // 不是按键事件（设备信息响应之类的帧也会从这个接口冒出来），
            // 所以它既不改变按住状态，也不该影响旋钮收尾的判定——那件事由时间窗决定。
            log::debug!(target: "hid", "Consumer 0x{key_value:04X} 不是已知键值，忽略");
            return ConsumerFrame {
                batches: Vec::new(),
                cleared: false,
                counts_as_key_event: false,
            };
        };

        let meta = PressedKeyMeta {
            key_index,
            key_name: get_key_name(key_index).to_string(),
            key_value,
            source: KeySource::Consumer,
        };

        if is_knob_pulse_key_index(key_index) {
            // 旋钮转动是编码器脉冲：连同按住的键报一次，立刻再报回按住集合，
            // 等价于「敲了一下」。关键是 held 一个都不动。
            log::debug!(
                target: "hid",
                "Consumer 0x{:04X} 旋钮脉冲 key{}（按住的 {:?} 保持不变）",
                key_value, key_index, self.held_indices()
            );
            let mut with_pulse = self.held.clone();
            with_pulse.push(meta);
            self.last_knob_pulse_at = Some(now);
            return ConsumerFrame {
                batches: vec![with_pulse, self.held.clone()],
                cleared: false,
                counts_as_key_event: true,
            };
        }

        if !self.held.iter().any(|m| m.key_index == key_index) {
            self.held.push(meta);
        }
        log::debug!(
            target: "hid",
            "Consumer 0x{:04X} 按下 key{}，当前按住 {:?}",
            key_value, key_index, self.held_indices()
        );
        // 真按下一个键，旋钮那一格的收尾就无从谈起了
        self.last_knob_pulse_at = None;
        ConsumerFrame {
            batches: vec![self.held.clone()],
            cleared: false,
            counts_as_key_event: true,
        }
    }

    /// 当前按住的键索引（只用于日志，别拿去做判断）。
    fn held_indices(&self) -> Vec<usize> {
        self.held.iter().map(|m| m.key_index).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::event::BoardEvent;
    use tokio::sync::broadcast;

    // ---- Consumer 通道的按住状态 ----
    //
    // 这条通道一次只能报一个键值，「按住 Tab 转旋钮」这种并发状态它表达不出来。
    // 下面这组用例锁的就是「怎么从单值报文里把并发状态还原出来」。
    //
    // 时序全部由 `at()` 构造，不 sleep：真机上「脉冲收尾」与「用户松手」只差在
    // 距离上一格旋钮多久（实测 1.4 毫秒 vs 几百毫秒），这正是要精确覆盖的维度。

    const TAB: u16 = 0x0F01;
    const ESC: u16 = 0x0F03;
    const KNOB_CW: u16 = 0x0F08;
    const RELEASE: u16 = 0x0000;

    /// 测试基准时刻。
    fn t0() -> Instant {
        Instant::now()
    }

    /// 基准时刻之后 `ms` 毫秒。
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// 取一帧上报出去的各批按键索引。
    fn batches_of(frame: &ConsumerFrame) -> Vec<Vec<usize>> {
        frame
            .batches
            .iter()
            .map(|b| b.iter().map(|m| m.key_index).collect())
            .collect()
    }

    #[test]
    fn knob_pulse_keeps_the_held_key() {
        // 这是整个改动的理由：按住 Tab 转旋钮，Tab 必须还在。
        // 少了这条，转一格切换器就被关掉，旋钮还会去触发它自己的绑定。
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        assert_eq!(batches_of(&tracker.on_frame(TAB, base)), vec![vec![3]]);

        // 旋钮进一次、出一次；两批里 Tab 都在
        let pulse = tracker.on_frame(KNOB_CW, at(base, 500));
        assert_eq!(batches_of(&pulse), vec![vec![3, 1], vec![3]]);
        assert!(!pulse.cleared);

        // 脉冲收尾的 0x0000（实测约 1.4 毫秒后到）不是「全部松开」，一批都不该上报
        let tail = tracker.on_frame(RELEASE, at(base, 502));
        assert!(batches_of(&tail).is_empty(), "脉冲收尾不该改变按住状态");
        assert!(!tail.cleared);

        // 再转一格，Tab 依然在
        assert_eq!(
            batches_of(&tracker.on_frame(KNOB_CW, at(base, 700))),
            vec![vec![3, 1], vec![3]]
        );
    }

    #[test]
    fn real_release_after_knob_still_clears() {
        // 反面：吞掉的只能是紧跟脉冲的那一帧。用户真松手时必须清干净，
        // 否则按住状态永远留着，长按会话再也结束不了。
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        tracker.on_frame(TAB, base);
        tracker.on_frame(KNOB_CW, at(base, 500));
        tracker.on_frame(RELEASE, at(base, 502)); // 脉冲收尾，吞掉

        // 真机抓到的松手：距最后一格旋钮 565 毫秒
        let released = tracker.on_frame(RELEASE, at(base, 1067));
        assert_eq!(batches_of(&released), vec![Vec::<usize>::new()]);
        assert!(released.cleared);
        assert!(tracker.held.is_empty());
    }

    #[test]
    fn one_pulse_only_swallows_one_tail() {
        // 一格旋钮只配一次收尾。真机上「脉冲 → 收尾 → 松手」三帧可能挨得很近
        // （松手那帧同样在 100 毫秒窗口内），此时不能连吞两帧，否则按住状态漏清。
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        tracker.on_frame(TAB, base);
        tracker.on_frame(KNOB_CW, at(base, 500));

        let tail = tracker.on_frame(RELEASE, at(base, 502));
        assert!(batches_of(&tail).is_empty(), "第一帧是脉冲收尾");

        let released = tracker.on_frame(RELEASE, at(base, 520));
        assert!(released.cleared, "紧接着的第二帧就是真松手，必须清空");
        assert!(tracker.held.is_empty());
    }

    #[test]
    fn stale_pulse_does_not_swallow_a_later_release() {
        // 时间窗过期后，0x0000 一律按真松手处理——按住状态不会被无限期挂起。
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        tracker.on_frame(TAB, base);
        tracker.on_frame(KNOB_CW, at(base, 500));

        let just_past_window = at(base, 500) + KNOB_PULSE_TAIL_WINDOW + Duration::from_millis(1);
        let released = tracker.on_frame(RELEASE, just_past_window);
        assert!(released.cleared, "超出收尾窗口的 0x0000 必须清空按住状态");
    }

    #[test]
    fn unknown_frame_between_pulse_and_tail_does_not_break_the_hold() {
        // 设备信息响应之类的未知帧会从这个接口冒出来（真机日志里出现过三帧）。
        // 它插在脉冲和收尾之间时，两个方向都不能错：
        // 既不能把收尾当成松手（按住状态当场丢），也不能让它遮住后面真正的松手。
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        tracker.on_frame(TAB, base);
        tracker.on_frame(KNOB_CW, at(base, 500));
        tracker.on_frame(0x3B13, at(base, 501)); // 未知帧插队

        let tail = tracker.on_frame(RELEASE, at(base, 502));
        assert!(batches_of(&tail).is_empty(), "未知帧不该让收尾被误判成松手");
        assert_eq!(tracker.held_indices(), vec![3]);

        let released = tracker.on_frame(RELEASE, at(base, 1100));
        assert!(released.cleared, "未知帧也不该遮住之后真正的松手");
    }

    #[test]
    fn second_key_is_added_not_replacing_the_first() {
        // 「按住 Tab 的同时按别的键」：单值通道里第二个键值直接顶掉第一个，
        // 但中间没有 0x0000，说明两个键真的重叠着——所以是追加。
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        tracker.on_frame(TAB, base);
        assert_eq!(
            batches_of(&tracker.on_frame(ESC, at(base, 100))),
            vec![vec![3, 5]]
        );

        let released = tracker.on_frame(RELEASE, at(base, 200));
        assert_eq!(batches_of(&released), vec![Vec::<usize>::new()]);
    }

    #[test]
    fn separate_presses_do_not_pile_up() {
        // 两次独立按键之间有 0x0000 分隔，不能被当成「一直按着」堆在一起。
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        tracker.on_frame(TAB, base);
        tracker.on_frame(RELEASE, at(base, 100));
        assert_eq!(
            batches_of(&tracker.on_frame(ESC, at(base, 200))),
            vec![vec![5]]
        );
    }

    #[test]
    fn same_key_repeated_does_not_duplicate() {
        // 固件可能重复上报同一个键值（状态型上报在脉冲后会重报按住的键）。
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        tracker.on_frame(TAB, base);
        assert_eq!(
            batches_of(&tracker.on_frame(TAB, at(base, 10))),
            vec![vec![3]]
        );
    }

    #[test]
    fn pressing_a_key_cancels_the_pending_pulse_tail() {
        // 旋钮之后紧接着按下一个键，再来的 0x0000 是这个键的松开，不是旋钮的收尾。
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        tracker.on_frame(KNOB_CW, base);
        tracker.on_frame(TAB, at(base, 5));

        let released = tracker.on_frame(RELEASE, at(base, 10));
        assert!(released.cleared, "按下过键之后，0x0000 必须按真松手处理");
        assert!(tracker.held.is_empty());
    }

    #[test]
    fn knob_alone_reports_a_tap_and_holds_nothing() {
        // 没有按住任何键时转旋钮：照旧一进一出，且不会留下按住状态。
        let mut tracker = ConsumerHeldTracker::default();
        let pulse = tracker.on_frame(KNOB_CW, t0());
        assert_eq!(batches_of(&pulse), vec![vec![1], Vec::<usize>::new()]);
        assert!(tracker.held.is_empty(), "旋钮不该被当成按住的键");
    }

    #[test]
    fn unknown_key_value_changes_nothing() {
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();
        tracker.on_frame(TAB, base);
        let unknown = tracker.on_frame(0xFFFF, at(base, 10));
        assert!(batches_of(&unknown).is_empty());
        assert_eq!(tracker.held_indices(), vec![3], "未知键值不该动按住状态");
    }

    /// 端到端锁死用户报的那个 bug：按住 Tab 转旋钮，聚合器**不能**发出 Tab 的释放事件。
    ///
    /// 单测 tracker 只能证明它自己的账对，但用户实际受害的是聚合器 diff 出来的
    /// 「Tab released」——那一下会让长按会话收尾、切换器被关掉。所以这条一直测到事件流。
    #[test]
    fn aggregator_never_releases_the_held_key_while_the_knob_turns() {
        use crate::kernel::key_aggregator::KeyStateAggregator;

        let (tx, mut rx) = broadcast::channel(64);
        let aggregator = KeyStateAggregator::new(tx);
        let base = t0();
        let mut tracker = ConsumerHeldTracker::default();

        // 按住 Tab，连转三格，再松手
        let frames = [
            (TAB, 0),
            (KNOB_CW, 300),
            (RELEASE, 302),
            (KNOB_CW, 450),
            (RELEASE, 452),
            (KNOB_CW, 600),
            (RELEASE, 602),
            (RELEASE, 1200),
        ];
        for (value, ms) in frames {
            for batch in tracker.on_frame(value, at(base, ms)).batches {
                aggregator.report_change(KeySource::Consumer, batch, None);
            }
        }

        // 只挑按键事件看。中间还会夹着 ComboKey（同时按着两个键时聚合器就会发），
        // 遇到它必须继续收而不是停下——上层 driver 明确忽略 ComboKey，这里也不该被它带偏。
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let BoardEvent::KeyPress(key) = event {
                events.push((key.key_index, key.pressed));
            }
        }

        // Tab 只该按下一次、松开一次——中间三格旋钮不能插进任何一次 Tab 释放
        let tab_events: Vec<bool> = events
            .iter()
            .filter(|(index, _)| *index == 3)
            .map(|(_, pressed)| *pressed)
            .collect();
        assert_eq!(
            tab_events,
            vec![true, false],
            "按住 Tab 转旋钮期间，Tab 不该被判成松开：{events:?}"
        );
        // 三格旋钮各自敲了一下
        let knob_presses = events
            .iter()
            .filter(|(index, pressed)| *index == 1 && *pressed)
            .count();
        assert_eq!(knob_presses, 3, "三格旋钮应各产生一次按下：{events:?}");
    }
}
