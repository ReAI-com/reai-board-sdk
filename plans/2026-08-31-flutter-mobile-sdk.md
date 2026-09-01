# Flutter 移动端 SDK 实施计划

## 任务目标

在 `reai-board-sdk` 仓库内新增一个 Flutter 专用包，使 iOS/Android App 不依赖桌面端
`btleplug`、`hidapi`、`cpal` 或 Tokio runtime，也能通过纯 BLE 连接 ReAI-Vibe-Board。
Flutter 包应以 Rust SDK 的公开门面和固件协议为权威，提供相同语义的连接、事件、命令和
板载音频控制能力，供 `reai_mobile_app` 后续直接依赖。

## 需求摘要

- 功能：Flutter 扫描、连接、断开、自动重连、命令/响应、按键与模式事件、设备信息、
  常用配置命令、音频 capability/lease 和原始 mSBC 帧。
- 范围：iOS/Android BLE；使用 `flutter_blue_plus`；包放在 `flutter/reai_board_sdk/`。
- 验收：Dart 单测覆盖协议与状态机，Flutter analyze/test 通过，Rust 原有测试与 release
  readiness 不回归，CI 同时验证 Rust 和 Flutter 包。
- 约束：保持当前固件 GATT 协议；不把手机强行塞进桌面 USB runtime；注释、文档、提交中文。

## 当前状态与架构约束

1. Rust `BoardDevice` 同时编排 USB/BLE；移动端只需要 BLE，而且 BLE 权限、后台行为和
   系统连接恢复应交由 Flutter 插件处理。
2. 固件 Vendor GATT 使用设备名前缀 `REAI_VB_`，四个 UUID 必须使用固件完整形式：
   `00000000-0000-0000-0000-00000000fe60/61/62/63`。它们不是 Bluetooth Base UUID
   展开的 16-bit `FE60~FE63`；SDK 内禁止用短 `Guid` 构造。命令包为 `[CMD][LEN][DATA...]`。
3. Event 通道既承载异步输入事件，也承载命令响应；同一 CMD 同时只允许一个等待者，写入
   必须串行化并在超时/断连时清理。
4. 音频需要 capability 0x6E 和 lease 0x6F；FE63 提供版本化或旧版 mSBC 包。
5. 仓库内 mSBC 解码器为 LGPL-2.1-or-later。首个 MIT Flutter 包不复制或静态编入该实现，
   只暴露带连续性元数据的编码帧和可插拔解码接口，避免未经决策扩大 App 分发许可义务。
6. Rust 与 Dart 不各自维护协议字面量：新增仓库级 JSON golden vectors，由 Rust 测试校验，
   Dart 测试读取同一文件。命令编码统一遵循 `HidPacket::* → hid_to_gatt_command` 两步语义。

## 范围

### 本次实现

1. Flutter 包公共模型、错误、事件和 `BoardDevice` 门面。
2. `BoardBleTransport` 抽象，以及基于 `flutter_blue_plus` 的默认实现。
3. 设备扫描、连接、服务发现、FE62/FE63 订阅、主动断开和平台化自动重连：
   - 首次发现统一匹配广播 `advName` 的 `REAI_VB_` 前缀；
   - iOS 持久化 peripheral identifier，断线后对已知设备使用 `autoConnect` 挂起连接，不依赖
     后台无过滤重扫；identifier 失效或从未连接时才前台重扫；
   - Android 对已知 remote id 直连，失败后用指数退避重扫兜底；
   - 进程被系统杀死后的恢复和 iOS 后台重新扫描分别作为非保证能力记录。
4. 串行命令队列、匹配响应、超时和断连清理。
5. 常用移动端 API：
   - `readDeviceInfo`
   - `readKeyConfig` / `writeKeyConfig`
   - `getWorkMode`
   - `getSilentRecord` / `setSilentRecord`
   - `getSleepTimeout` / `setSleepTimeout`
   - `notifyAppOnline` / `getAppOnline`
   - `getOpenUrl` / `setOpenUrl`
   - `queryAudioCapabilities` / `controlAudioStream`
6. 按键、AI 语音键、组合键和模式事件；断连时补释放，避免上层按键卡住。
7. 版本化/旧版音频包解析、sequence gap/discontinuity 元数据、路由门控。
8. 显式 `startLegacySessionAudio()` 兼容旧固件：只开放 session 音频，不伪造
   capability/lease，不允许 timeline；与版本化 lease 路径互斥。
9. 示例 App、README 中英文说明、Rust/Flutter 接口对照表、CI。

### 明确不做

1. USB、USB 抢占、桌面热插拔和 USB DFU。
2. 工厂测试命令。
3. 在 MIT Flutter 包中内置 LGPL mSBC 到 PCM 解码器。
4. 本 PR 直接修改 `reai_mobile_app` 或后端数据库。
5. 声称完成实机验收；没有真实 Board 和手机交互证据时只报告自动验证。
6. 绑定配置块 `readBindingsBlob` / `writeBindingsBlob`（0x69/0x6A）：它需要分片、CRC、
   commit 和回读校验，首版先交付当前 App 连接/输入/音频主链路，后续独立补齐。

## 目录与模块

```text
flutter/reai_board_sdk/
  pubspec.yaml
  analysis_options.yaml
  lib/reai_board_sdk.dart
  lib/src/constants.dart
  lib/src/models.dart
  lib/src/events.dart
  lib/src/errors.dart
  lib/src/protocol.dart
  lib/src/key_state.dart
  lib/src/clock.dart
  lib/src/transport.dart
  lib/src/flutter_blue_plus_transport.dart
  lib/src/board_device.dart
  test/*_test.dart
  example/...
  README.md
```

根目录同步修改：`.github/workflows/ci.yml`、`.gitignore`、`Cargo.toml`、`README.md`、
`README.zh-CN.md`、`CHANGELOG.md`、`tests/flutter_protocol_vectors.rs`、
`tests/fixtures/flutter_protocol_vectors.json`。本次不放宽 `scripts/check-release-readiness.sh`；
新增文档统一使用其要求的产品名 `ReAI-Vibe-Board`，并执行现有 readiness 检查。

## 公共接口原则

1. Dart 命名遵循 Dart 风格，但语义和 Rust 门面一一对照，例如 `readDeviceInfo()`、
   `events`、`queryAudioCapabilities()`、`controlAudioStream()`。
2. `BoardDevice` 依赖 `BoardBleTransport`，业务层不导入 `flutter_blue_plus` 类型。
3. 扫描结果使用 SDK 自有 `BleDeviceInfo`；只有默认 transport 内部保存插件句柄。
4. `start()` 负责监听 transport 生命周期；`connect()` 负责用户选择后的连接；`shutdown()`
   best-effort 上报离线并释放订阅。
5. 状态变化用广播 Stream；命令 Future 只对应一次请求；错误分为 transport、protocol、
   timeout、disconnected 和 unsupported。
6. 音频流先输出 `EncodedAudioFrame`。帧携带 transport、sequence、设备 discontinuity、
   sequence gap、connection epoch，供 App 的解码层和听写管道使用。
7. `AudioCapabilityState` 与 Rust 保持三态：`unqueried`、`unavailable`、`ready`；每次连接
   epoch 变化都重置，不能把“没查过”和“固件不支持”都压成 null。
8. SDK 不自动续租，保持 Rust 门面语义：App 按 TTL 调 Heartbeat。Stop、LeaseMismatch 或
   owner/scope/lease 回包不匹配时立即关音频门控并递增 route epoch。
9. `ConsumerHeldTracker` 的 `now`、命令超时、重连退避和通知稳定等待均使用可注入时钟/
   Timer 工厂；测试用 fake time，不使用 sleep。

## 实施顺序

1. 创建最新 `origin/main` 基线的隔离 worktree。
2. 先增加 Rust/Dart 共用 JSON golden vectors，并写测试和 fake transport：
   - GATT 常量、命令编码、响应解析；
   - 设备信息/配置/音频解析；
   - 并发命令串行和超时；
   - 异步事件与 pending 响应分流；
   - 断连释放、重连和音频门控。
3. 实现纯 Dart 协议、模型和 `BoardDevice`。
4. 实现尽量薄的 `flutter_blue_plus` transport，只负责 I/O：扫描用 `advName`，通知使用
   `onValueReceived`，先建立监听再 `setNotifyValue(true)`，连接状态来自 `connectionState`。
   状态机、重连决策、解析全部保留在可测试的纯 Dart 层。
5. 连接后记录 `maxCommandPayload`；Android 主动请求 MTU ≥247，iOS 读取系统协商值。
   MTU 不足不阻断短事件和短命令进入 ready，而是发出可观测的 capability warning；
   `readDeviceInfo`、`readKeyConfig`、`writeKeyConfig` 和启用音频在调用点检查并抛 typed error。
6. FE62/FE63 订阅完成后通过可配置的 500ms 稳定门禁再发送第一条命令；pending 响应优先于
   同 CMD 异步推送，严格锁定 Rust 当前行为。
7. 增加最小 example，展示扫描、选择连接、设备信息、事件和音频 lease；example 真实配置
   iOS/Android 蓝牙权限和 iOS `UIBackgroundModes=bluetooth-central`，并提供前后台真机冒烟
   checklist。文档区分前台恢复保证与“接入 App 开启后台模式且进程存活”时的后台恢复。
8. 更新中英文文档、功能矩阵、权限片段与许可证边界。明确本交付是依赖 Flutter/FBP 的
   Flutter library，不带自有原生 plugin，也不会替接入 App 注入 Info.plist/Manifest。
9. `Cargo.toml` 排除 `flutter/**`，避免 crates.io 打包 Flutter 内容；example 采用
   `flutter create` 标准 `.gitignore`，至少排除 `android/local.properties`、`android/.gradle/`、
   `ios/Flutter/Generated.xcconfig`、`ios/Flutter/flutter_export_environment.sh`、`ios/Pods/`、
   `.dart_tool/`、`build/`。库包不提交 lock，example 提交 lock；PR 前用 `git ls-files`
   确认没有机器路径文件被跟踪。
10. CI 增加 Flutter stable 的库包 `pub get`、`format --set-exit-if-changed`、`analyze`、
    `test`，以及 example 独立 `pub get`、`analyze`；golden fixture 路径不存在必须硬失败。
11. 运行 Flutter 定向/全量测试、Rust 全量回归和 release readiness，审查 diff。
12. 中文 Conventional Commit，推送并创建 PR；完成 Codex PR review，不自动合并。

## 测试矩阵

### 协议

- 四个全零基 UUID 字面量与 `REAI_VB_` 前缀固定；短 UUID/Base UUID 展开值明确不相等。
- Rust 校验 JSON golden，Dart 读取同一文件；任一侧协议变化都会让 CI 失败。
- HID 64 字节命令正确转换为 GATT 可变长包。
- 短包、声明长度越界、错误 CMD、错误 result 被拒绝。
- device info、key config、work mode、静默录音、sleep timeout、App online、open URL、
  audio capability/stream ack 与 Rust fixture 一致。
- 版本化音频只接受 57 字节倍数；旧格式按可用长度钳制。
- 最大 62 字节 key config GATT 命令编码正确；协商载荷不足时短事件仍工作，设备信息、
  按键配置和音频启用分别在调用点报 typed MTU error。

### 生命周期与并发

- 连接成功后先订阅 FE62/FE63，再上报 online。
- 同时发起两个命令时严格串行，不发生同 CMD pending 覆盖。
- 超时、写失败、断连会完成并清空等待 Future。
- 主动断开不自动重连；意外断开按配置重连并发 Reconnect 事件。
- shutdown 幂等，所有 StreamSubscription/Timer/Controller 可释放。
- `CMD_STATUS 0x12` 在 pending 存在时优先作为响应，不同时发异步 ModeChange。
- 使用 fake time 验证 500ms 通知稳定门禁、命令超时和重连退避，不产生慢测/偶发测。

### 输入与音频

- 旋钮脉冲成对 press/release。
- 按住普通键转旋钮，不误释放普通键。
- 按住 AI 语音键转旋钮，不误停止语音键。
- 断连补齐所有按键和 AI 语音键释放。
- 未获得音频 lease 时丢弃 FE63；路由 epoch 变化后旧帧不串流。
- sequence gap 和设备 discontinuity 分开表达。
- capability 三态在连接 epoch 变化后重置；unqueried/unavailable 行为不同。
- 旧固件只有显式 legacy session 入口能打开门控，不能用于 timeline。
- LeaseMismatch、Stop 或 owner/scope/lease 不匹配时不得开门，已开门则立即关闭。

### 构建回归

- `dart format --output=none --set-exit-if-changed .`
- `flutter analyze`
- `flutter test`
- 使用稳定 Rust：`cargo fmt --all -- --check`、`cargo test --workspace --all-features`、
  `./scripts/check-release-readiness.sh`。

## 风险与边界

1. UUID 只接受固件全 128 位形式，统一小写比较但绝不把短 Bluetooth Base UUID 当等价值。
2. iOS 后台重连依赖已持久化 peripheral identifier + autoConnect，并要求接入 App 声明
   `UIBackgroundModes=bluetooth-central` 且进程存活；进程被杀后恢复和后台无过滤扫描不保证。
3. 固件 Event 通道没有请求 ID，只能串行命令；不能通过并发优化破坏这一约束。
4. BLE 无应用层身份认证/加密；文档必须提醒持久化写操作由 App 做用户确认。
5. `flutter_blue_plus` 先约束为 `>=1.35.2 <1.40.0`，README 列出依赖的 `advName`、
   `onValueReceived`、`connectionState`、id 恢复和 `autoConnect` API；升级时逐项核对。
6. 无实机时无法证明 Android/iOS 权限、吞吐、后台和固件版本兼容，PR 明确列为验收缺口。
7. `flutter_blue_plus` 适配器无法由 fake transport 覆盖；保持其无状态/薄 I/O，并在 PR 中
   单列自动化覆盖缺口和可执行真机 checklist。
8. Android 12+ 需要 BLUETOOTH_SCAN/CONNECT；Android 11 及以下需要定位权限和系统定位
   开关；iOS 需要 NSBluetoothAlwaysUsageDescription、兼容旧 deployment target 的
   NSBluetoothPeripheralUsageDescription，以及后台连接所需 bluetooth-central。包不注入权限，
   example 和 README 给全，并提醒 App Store 审核需要解释后台蓝牙用途。

## 回滚

- 合并前：移除 worktree，保留分支和计划记录。
- 合并后：revert Flutter SDK PR；Rust crate 不改变现有 API，回滚不会影响桌面消费者。
- 新 CI job 可单独 revert；没有数据库、固件或线上数据 migration。

## 完成标准

1. Flutter 包可被 path dependency 引用，公共示例通过独立 `pub get` 和 `analyze`。
2. fake transport 测试证明 Dart 与 Rust 实现逐字节一致，并覆盖并发、生命周期和主要事件语义；
   golden 不能替代固件真机验收，fixture 缺失必须失败而不是 skip。
3. Rust 现有测试无回归。
4. README 清楚说明能力、非目标、许可证和手机权限。
5. PR 已创建，CI 通过或只剩明确的外部环境阻塞；PR review 无未处理高优先级问题。
