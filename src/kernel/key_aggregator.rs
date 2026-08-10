//! 跨接口键状态聚合器
//!
//! 合并 0x02 Config 和 0x0C Consumer 两个 HID 接口的按键状态,
//! 通过 `broadcast::Sender<BoardEvent>` 广播按键/组合键事件。
//!
//! 被 Config / Consumer 两个监控线程共享(通过 `Arc`)。所有可变状态由单把
//! Mutex 保护,避免嵌套锁死锁。事件构造在锁内完成,广播在锁外发送。

use std::collections::HashMap;
use std::sync::Mutex;

use smallvec::SmallVec;
use tokio::sync::broadcast;

use super::protocol_hid::get_key_name;
use crate::kernel::event::{BoardEvent, ComboKeyEvent, KeyPressEvent, KeySource};

/// 按键元数据(按下时记录,释放时复用)
///
/// `key_value` 语义:
/// - Config 接口 (0x02):整个 bitmask(如 0x03 = KEY6+KEY7)
/// - Consumer 接口 (0x0C):HID Usage Code(如 0x0F01 = Tab)
/// - 释放事件:使用 `0x0000` 作为释放标志
#[derive(Debug, Clone)]
pub struct PressedKeyMeta {
    pub key_index: usize,
    pub key_name: String,
    pub key_value: u16,
    pub source: KeySource,
}

/// 聚合器内部状态(单把 Mutex 保护,避免嵌套死锁)
struct AggState {
    /// 每个接口当前按下的键及其元数据
    state: HashMap<KeySource, HashMap<usize, PressedKeyMeta>>,
    /// 上一次全局按键快照。
    ///
    /// 用 `SmallVec<[usize; 12]>` 而非 `HashSet<usize>`:按键总数固定为 12
    /// (`ACTIVE_KEY_COUNT`),内联容量正好覆盖,避免每次 report_change 在热路径
    /// 上分配 HashSet bucket 数组。diff 用线性扫描(O(n²),12 元素只 144 次比较,
    /// 远低于 HashSet 的 hashing 开销)。
    prev_all_pressed: SmallVec<[usize; 12]>,
    /// 最近一次 Config 来源的原始 bitmask
    last_config_mask: Option<u8>,
}

/// 跨接口键状态聚合器
///
/// 合并 Config / Consumer 两个接口的按键状态,通过 broadcast 广播。
pub struct KeyStateAggregator {
    inner: Mutex<AggState>,
    event_tx: broadcast::Sender<BoardEvent>,
}

impl KeyStateAggregator {
    /// 创建聚合器
    pub fn new(event_tx: broadcast::Sender<BoardEvent>) -> Self {
        Self {
            inner: Mutex::new(AggState {
                state: HashMap::new(),
                prev_all_pressed: SmallVec::new(),
                last_config_mask: None,
            }),
            event_tx,
        }
    }

    /// 报告某个来源的按键状态变化
    ///
    /// 调用方传入该来源**当前**按下的全部键。释放时传空 `Vec`。
    pub fn report_change(
        &self,
        source: KeySource,
        pressed_keys: Vec<PressedKeyMeta>,
        config_mask: Option<u8>,
    ) {
        // 在锁内完成所有状态计算,收集要广播的事件,锁外再 send(避免 send 阻塞监控线程)
        //
        // 用 unwrap_or_else(|e| e.into_inner()) 而非 unwrap():某次 panic 让 Mutex
        // 中毒后,直接 unwrap 会让后续所有按键事件都二次 panic,整个按键流瘫痪。
        // into_inner 取出中毒锁内的数据,允许恢复(可能用过期状态,但优于崩溃)。
        let (released_events, pressed_events, combo_event) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

            // 保存 Config 来源的 bitmask
            if source == KeySource::Config {
                inner.last_config_mask = config_mask;
            }

            // 构建新的来源状态并替换
            let new_source_map: HashMap<usize, PressedKeyMeta> =
                pressed_keys.into_iter().map(|m| (m.key_index, m)).collect();
            inner.state.insert(source, new_source_map);

            // 全局按键集合与差集。
            // all_pressed/newly_pressed/released 都用 SmallVec 内联(≤12 元素),
            // 避免 HashSet 的 bucket 分配;diff 走线性扫描,12 元素下远快于 hashing。
            let mut all_pressed: SmallVec<[usize; 12]> = inner
                .state
                .values()
                .flat_map(|m| m.keys())
                .copied()
                .collect();
            all_pressed.sort();
            all_pressed.dedup();

            let mut newly_pressed: SmallVec<[usize; 12]> = SmallVec::new();
            for &k in &all_pressed {
                if !inner.prev_all_pressed.contains(&k) {
                    newly_pressed.push(k);
                }
            }
            let mut released: SmallVec<[usize; 12]> = SmallVec::new();
            for &k in &inner.prev_all_pressed {
                if !all_pressed.contains(&k) {
                    released.push(k);
                }
            }

            // 排序后的当前按键列表(all_pressed 已排序)
            let all_sorted: Vec<usize> = all_pressed.iter().copied().collect();

            // 释放事件(被移除的键来自当前报告的 source —— state[source] 被整体替换)
            let mut rel_events: Vec<KeyPressEvent> = Vec::new();
            for &key_index in &released {
                rel_events.push(KeyPressEvent {
                    key_index,
                    key_name: get_key_name(key_index).to_string(),
                    key_value: 0x0000,
                    pressed: false,
                    source,
                });
            }

            // 按下事件(升序)—— newly_pressed 已是 all_pressed 子集,排序保持稳定
            let mut newly_sorted: Vec<usize> = newly_pressed.iter().copied().collect();
            newly_sorted.sort();

            let mut press_events: Vec<KeyPressEvent> = Vec::new();
            for &key_index in &newly_sorted {
                if let Some(meta) = Self::find_meta(&inner.state, key_index) {
                    press_events.push(KeyPressEvent {
                        key_index: meta.key_index,
                        key_name: meta.key_name.clone(),
                        key_value: meta.key_value,
                        pressed: true,
                        source: meta.source,
                    });
                }
            }

            // Combo 事件(同时按下 ≥2 键且有新按下)
            let combo = if all_pressed.len() > 1 && !newly_sorted.is_empty() {
                Some(ComboKeyEvent {
                    keys: all_sorted.clone(),
                    key_names: all_sorted
                        .iter()
                        .map(|&i| get_key_name(i).to_string())
                        .collect(),
                    config_mask: inner.last_config_mask,
                })
            } else {
                None
            };

            inner.prev_all_pressed = all_pressed;
            (rel_events, press_events, combo)
        };
        // 锁已释放

        for evt in released_events {
            let _ = self.event_tx.send(BoardEvent::KeyPress(evt));
        }
        for evt in pressed_events {
            log::debug!(target: "hid", "键按下: key_index={}", evt.key_index);
            let _ = self.event_tx.send(BoardEvent::KeyPress(evt));
        }
        if let Some(combo) = combo_event {
            let _ = self.event_tx.send(BoardEvent::ComboKey(combo));
        }
    }

    /// 在所有来源的状态中查找某个 key_index 的元数据
    fn find_meta(
        state: &HashMap<KeySource, HashMap<usize, PressedKeyMeta>>,
        key_index: usize,
    ) -> Option<&PressedKeyMeta> {
        state.values().find_map(|m| m.get(&key_index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::protocol_hid::get_key_name;

    fn meta(idx: usize, val: u16, source: KeySource) -> PressedKeyMeta {
        PressedKeyMeta {
            key_index: idx,
            key_name: get_key_name(idx).to_string(),
            key_value: val,
            source,
        }
    }

    /// 非阻塞 drain broadcast 缓冲的事件(测试用,避免 blocking_recv 阻塞)
    fn drain(rx: &mut broadcast::Receiver<BoardEvent>) -> Vec<BoardEvent> {
        let mut v = Vec::new();
        while let Ok(e) = rx.try_recv() {
            v.push(e);
        }
        v
    }

    #[test]
    fn press_and_release_single_key() {
        let (tx, mut rx) = broadcast::channel(64);
        let agg = KeyStateAggregator::new(tx);

        // Config 接口按下 KEY6
        agg.report_change(
            KeySource::Config,
            vec![meta(6, 0x01, KeySource::Config)],
            Some(0x01),
        );
        let evts = drain(&mut rx);
        assert_eq!(evts.len(), 1);
        match &evts[0] {
            BoardEvent::KeyPress(k) => {
                assert_eq!(k.key_index, 6);
                assert!(k.pressed);
                assert_eq!(k.source, KeySource::Config);
                assert_eq!(k.key_value, 0x01);
            }
            _ => panic!("expected KeyPress"),
        }

        // 释放
        agg.report_change(KeySource::Config, vec![], None);
        let evts = drain(&mut rx);
        assert_eq!(evts.len(), 1);
        if let BoardEvent::KeyPress(k) = &evts[0] {
            assert_eq!(k.key_index, 6);
            assert!(!k.pressed);
            assert_eq!(k.key_value, 0x0000);
        } else {
            panic!("expected KeyPress release");
        }
    }

    #[test]
    fn combo_when_two_keys_pressed() {
        let (tx, mut rx) = broadcast::channel(64);
        let agg = KeyStateAggregator::new(tx);

        // 同时按 KEY6 + KEY7
        agg.report_change(
            KeySource::Config,
            vec![
                meta(6, 0x01, KeySource::Config),
                meta(7, 0x02, KeySource::Config),
            ],
            Some(0x03),
        );
        let evts = drain(&mut rx);

        let presses: usize = evts
            .iter()
            .filter(|e| matches!(e, BoardEvent::KeyPress(k) if k.pressed))
            .count();
        let combos: Vec<&ComboKeyEvent> = evts
            .iter()
            .filter_map(|e| match e {
                BoardEvent::ComboKey(c) => Some(c),
                _ => None,
            })
            .collect();

        assert_eq!(presses, 2, "应有 2 个按下事件");
        assert_eq!(combos.len(), 1, "应有 1 个组合键事件");
        assert_eq!(combos[0].keys, vec![6, 7]);
        assert_eq!(combos[0].config_mask, Some(0x03));
    }

    #[test]
    fn cross_source_merge() {
        // Config 按 KEY6,Consumer 按 KEY3 —— 不应互相覆盖
        let (tx, mut rx) = broadcast::channel(64);
        let agg = KeyStateAggregator::new(tx);

        agg.report_change(
            KeySource::Config,
            vec![meta(6, 0x01, KeySource::Config)],
            Some(0x01),
        );
        agg.report_change(
            KeySource::Consumer,
            vec![meta(3, 0x0F01, KeySource::Consumer)],
            None,
        );
        let evts = drain(&mut rx);

        // 应有 2 个按下事件(KEY6 Config + KEY3 Consumer),无 combo(分两次报告)
        let pressed_indices: Vec<usize> = evts
            .iter()
            .filter_map(|e| match e {
                BoardEvent::KeyPress(k) if k.pressed => Some(k.key_index),
                _ => None,
            })
            .collect();
        assert_eq!(pressed_indices, vec![6, 3]);
    }
}
