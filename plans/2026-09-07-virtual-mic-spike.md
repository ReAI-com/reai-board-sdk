# virtual-mic spike 报告（2026-09-07）

> **状态：已完成并入 `virtual-mic` feature（2026-09-08 实地测试通过）。**
> 实测新增结论与当初 spike 的差异见文末「实施后记」。

Issue: reai-board-sdk#11 · 分支: `feat/virtual-mic`

**结论：macOS 虚拟麦克风全链路验证通过。** libASPL（v3.1.2，MIT）示例驱动 SinewaveDevice
在 macOS 26.5 arm64 上编译、安装、加载、出声全部成功；三个 spike 未知项都有了确定答案，
无阻塞性风险，可以进入正式实现。

## Spike 三问三答

### 1. HAL 插件能否加载？——能，adhoc 签名即可

- `cmake + make examples` 产出 `SinewaveDevice.driver`（CFBundle，arm64）。
- **adhoc（linker-signed）签名直接被 coreaudiod 加载**（macOS 26.5.1 arm64 实测），
  本地开发不需要 Developer ID。发行给用户时建议仍用 Developer ID 签名以稳妥，
  但不存在"未签名就死"的硬门槛（老版本 macOS 待回归验证）。
- 驱动内 trace 走 os_log，`log stream --predicate 'sender == "SinewaveDevice"'` 可观测。

### 2. TCC 麦克风权限如何表现？——虚拟麦克风照常受 TCC 管辖，消费者必须主动申请

这是本次 spike 最重要的发现，排查路径有普适参考价值：

| 消费方形态 | 行为 |
|---|---|
| CLI 工具直接用 AVAudioEngine 录 | **静默全零**，无弹窗（AVAudioEngine 不触发 TCC 询问） |
| 带 `NSMicrophoneUsageDescription` 的真 .app，先 `AVCaptureDevice.requestAccess(for: .audio)` | 正常弹窗 → 允许后立即出真实数据 |

- 未授权（notDetermined）时：设备照常 AddClient、StartIO、显示 Running，
  但 coreaudiod 不向驱动拉数据（驱动 `OnReadClientInput` 一次都不被调），
  客户端拿到全零——**现象不是"失败"而是"静音"，极易误判为驱动 bug**。
- 对 SDK 的含义：virtual-mic 的示例与文档必须写明"消费方 App 需要麦克风权限并主动 request"，
  这是苹果对所有输入设备（含虚拟）的统一行为，不是我们设备特有的限制。
- 授权归属"责任 App"：后台守护/shell 宿主无法弹窗时 request 直接返回 false。

### 3. 设备呈现如何？——系统级完整可见，但默认设备劫持要注意

- `system_profiler SPAudioDataType` 出现完整条目（Input Channels / SampleRate / Transport: Virtual）。
- **安装重启 coreaudiod 后被系统自动选为 Default Input Device** ——正式实现必须处理：
  设为不可作为默认（或至少文档提示），避免劫持用户系统输入。

## 实测操作记录（可复现）

```bash
git clone https://github.com/gavv/libASPL && cd libASPL   # 需完整历史（tag 版本提取），勿 --depth 1
make examples                        # 产物 build/Examples/*/​*.driver
sudo cp -fr build/Examples/SinewaveDevice/SinewaveDevice.driver /Library/Audio/Plug-Ins/HAL/
sudo killall coreaudiod              # 注意：osascript 提权环境下 launchctl kickstart 被 SIP 拦（150 错误），killall 可行
# 验证
system_profiler SPAudioDataType | grep -A6 Sinewave
# 录音验证：真 .app + NSMicrophoneUsageDescription + requestAccess → 3s 48k 1ch，rms=0.0089，AUDIO FLOWING
```

卸载：`sudo rm -rf "/Library/Audio/Plug-Ins/HAL/SinewaveDevice.driver" && sudo killall coreaudiod`

## 对正式实现的修正与输入

1. **数据面确认**：HAL 插件 loopback 架构成立；SDK 的 `PcmSink` → cpal 写设备输出侧 →
   输入侧出流。libASPL 的 `NetcatDevice` 示例就是"外部喂数据"骨架。
2. **设备参数**：声明 48 kHz mono（SDK 内 16k→48k 重采样）；正式驱动名 "ReAI Vibe Board"；
   音量/静音控制保留（libASPL `AddStreamWithControlsAsync` 自带）。
3. **安装器**：拷 `.driver` + `killall coreaudiod`；提权走 osascript（launchctl kickstart 不可用）。
4. **文档义务**：消费方需麦克风权限（requestAccess）——写进 README 与 example。
5. **许可**：libASPL 为 MIT + Apple 示例代码许可（LICENSE.apple2012/2020），与 SDK MIT 兼容。

## 下一步

- [x] 定 libASPL 集成方式 → **vendor 进仓**（`virtual-mic/libASPL`，build.rs 经 cmake 子构建，无系统安装依赖）
- [x] `virtual-mic` Cargo feature + `src/virtual_mic/`（`ensure_installed`/`uninstall`/`start` + `PcmSink` UDP 泵）
- [x] ReAI 驱动（`virtual-mic/driver/`，arm64+x86_64）
- [x] example：`virtual_mic_demo.rs`（BLE → VirtualMic）
- [x] README 章节：安装要求、权限说明、16k 音质预期

## 实施后记（2026-09-08，与 spike 结论的差异）

1. **架构简化**：放弃了 spike 时设想的「HAL 插件 loopback 对 + cpal 写输出侧 +
   16k→48k 重采样」，改为 **UDP 喂入的纯输入设备**（NetcatDevice 反用）：
   驱动只暴露一个 16 kHz mono 输入设备并监听 `127.0.0.1:47160`，SDK 侧
   `VirtualMic` 只是实现 `PcmSink` 发 UDP 包。收益：SDK 零新依赖（不需要 cpal、
   不需要重采样——设备原生 16k，App 侧由系统自动转换），系统里只有一个干净的
   输入设备。
2. **`CanBeDefault` 教训（重要）**：设为 `false` 防默认劫持的结果是**设备从系统
   设置的输入面板里消失**（面板只列出可作默认的设备），CoreAudio 层仍可见，
   极易误判。必须为 `true`；实际劫持风险不存在——macOS 不会因新设备出现而自动
   切换已选定的默认输入（spike 时 Sinewave 上位只因为它当时是唯一输入设备）。
3. **BLE-first**：SDK 热插拔的自动连接**故意**不按 REAI_VB_ 前缀盲连（防多板
   误连），纯 BLE 场景必须 `scan_ble_devices()` → `connect_ble(name)`。新增
   `examples/ble_scan.rs` 裸扫诊断，用于区分「板子没广播」和「进程无蓝牙权限」。
4. **实地验收**：BLE mSBC → 解码 → 虚拟麦克风 → 系统输入电平随说话起伏 →
   App 录音正常。macOS 全链路打通。
