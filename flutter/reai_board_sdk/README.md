# reai_board_sdk for Flutter

面向 iOS / Android 的 ReAI-Vibe-Board BLE SDK。它保留 Rust SDK 的
`BoardDevice`、类型化命令、`BoardEvent` 和板载音频 lease 语义，但把手机系统 BLE
交给 `flutter_blue_plus`，不把桌面端的 `btleplug`、`hidapi`、`cpal` 或 Tokio runtime
带进 App。

## 安装

发布到 pub.dev 之前可直接引用 Git 仓库子目录：

```yaml
dependencies:
  reai_board_sdk:
    git:
      url: https://github.com/ReAI-com/reai-board-sdk.git
      path: flutter/reai_board_sdk
```

本地联调可使用 `path`：

```yaml
dependencies:
  reai_board_sdk:
    path: ../reai-board-sdk/flutter/reai_board_sdk
```

## 最小用法

```dart
final board = BoardDevice(
  transport: FlutterBluePlusBoardTransport(),
);
await board.start();

// 扫描过程中每发现或更新一台设备就会推送快照，不用等 10 秒超时。
board.scanResults.listen((devices) => print('found=${devices.length}'));

board.events.listen((event) {
  switch (event) {
    case KeyPressEvent():
      print('key=${event.keyIndex} pressed=${event.pressed}');
    case ModeChangeEvent():
      print('mode=${event.mode.label}');
    default:
      break;
  }
});

final devices = await board.scan();
await board.connect(devices.first);
final info = await board.readDeviceInfo();
print('${info.chipId} ${info.firmwareVersion} ${info.batteryLevel}%');
```

`BoardDevice` 负责：

- 扫描 `REAI_VB_` 广播名、连接、断开与已知 peripheral id 重连；
- `systemDevices(serviceUuid: FE60)` 优先接管 iOS 系统已连接设备，再以完整身份回读校验；
- `scanResults` 逐步推送扫描快照，`scan()` 同时保留最终列表返回值；
- FE61 串行命令，FE62 响应/异步事件分流，FE63 音频帧；
- `readDeviceInfo`、按键配置、模式、静默录音、休眠、App online、URL；
- `KeyConfig.bindings` 解析 20 个三字节槽位，`activeBindings` 暴露 12 个实体键；
- `BoardPhysicalKey`、`KeyBinding.description` 和 `copyWithActiveBinding` 支持移动端安全展示、局部合并；
- 音频 capability 三态、lease start/heartbeat/stop、sequence gap；
- 断连补发按键和 AI 语音键释放，避免上层出现“按住不放”。

外脑库存/绑定身份统一使用 `normalizeVibeBoardHardwareId(info.macAddress)`，
结果形如 `REAI_VB_CC8A2B197CF0`。广播短名和 peripheral UUID 不是硬件身份。

## Rust 接口对照

| Rust | Flutter | 说明 |
|---|---|---|
| `BoardDevice::events()` | `BoardDevice.events` | 广播事件流 |
| `scan_ble_devices()` | `scan()` | 手机只扫描 BLE |
| `connect_ble()` | `connect()` | 使用 SDK 自有 `BleDeviceInfo` |
| `read_device_info()` | `readDeviceInfo()` | `chip_id`、固件、电量 |
| `read/write_key_config()` | `read/writeKeyConfig()` | 需要足够 MTU |
| `get_work_mode()` | `getWorkMode()` | CHAT / YOLO / PLAN |
| `query_audio_capabilities()` | `queryAudioCapabilities()` | unqueried / unavailable / ready |
| `control_audio_stream()` | `controlAudioStream()` | App 自己负责 heartbeat |
| `start_legacy_ble_session_reader()` | `startLegacySessionAudio()` | 旧固件，仅 session |
| USB / DFU / factory test | 不提供 | 手机 BLE 包不伪装桌面能力 |
| bindings blob 0x69/0x6A | 首版不提供 | 后续按分片 + CRC + 回读独立实现 |

Rust 和 Dart 共读 `tests/fixtures/flutter_protocol_vectors.json`。fixture 不存在会直接测试失败，
避免两套协议在重构时静默漂移。

`example/` 是手机真机验收台：增量扫描、设备/MTU 状态、12 键实时高亮与计数、
按键配置读取/标脏/确认写入/回读验证、模式拨杆、音频 lease/连续帧和可导出的系统日志。
写入前会重读设备最新配置，只覆盖用户修改过的有效槽位，未知 Class 和桌面脚本绑定保持原样。

## 音频边界

Flutter 包输出 `EncodedAudioFrame`（mSBC、16 kHz mono、57 字节/帧）以及 sequence、
device discontinuity、connection epoch 和 route epoch。它不在 MIT 包里复制或静态编入仓库的
LGPL-2.1-or-later 解码器。App 可以接入经过许可证评估的独立解码器，再把 PCM16 送进自身
听写管道。

版本化音频必须先查询 capability，再建立短 TTL lease：

```dart
final caps = await board.queryAudioCapabilities();
final ttl = caps.defaultTtlMs;
await board.controlAudioStream(
  action: AudioStreamAction.start,
  scope: AudioStreamScope.session,
  leaseId: 0x4D4F424C,
  ttlMs: ttl,
);
// 调用方需在 TTL 到期前发送 heartbeat，并在结束时 stop。
```

iOS 连接刚建立时可能先报 ATT MTU 23（有效载荷 20 字节），系统会继续自动协商。
SDK 持续订阅 MTU 变化；受 MTU 限制的设备信息、按键配置和版本化音频操作会先等待
`BoardConfig.mtuNegotiationTimeout`，只有协商后仍不足才抛出 `BoardMtuException`。

## 权限

本包是 Flutter library，不含自有原生 plugin，也不会替宿主 App 修改权限。

### iOS `Info.plist`

```xml
<key>NSBluetoothAlwaysUsageDescription</key>
<string>用于连接 ReAI-Vibe-Board、接收按键事件和板载音频。</string>
<key>NSBluetoothPeripheralUsageDescription</key>
<string>用于兼容旧版 iOS 的蓝牙授权。</string>
<key>UIBackgroundModes</key>
<array>
  <string>bluetooth-central</string>
</array>
```

`bluetooth-central` 用于进程存活时的后台连接回调，提交 App Store 时需如实说明后台蓝牙用途。
前台恢复是 SDK 主路径；进程被系统杀死后的恢复和 iOS 后台无过滤扫描不保证。

### Android `AndroidManifest.xml`

```xml
<uses-feature android:name="android.hardware.bluetooth_le" android:required="false" />
<uses-permission android:name="android.permission.BLUETOOTH" android:maxSdkVersion="30" />
<uses-permission android:name="android.permission.BLUETOOTH_ADMIN" android:maxSdkVersion="30" />
<uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" android:maxSdkVersion="30" />
<uses-permission android:name="android.permission.BLUETOOTH_SCAN"
    android:usesPermissionFlags="neverForLocation" />
<uses-permission android:name="android.permission.BLUETOOTH_CONNECT" />
```

Android 12+ 还需运行时申请 `BLUETOOTH_SCAN` / `BLUETOOTH_CONNECT`。Android 11 及以下
扫描需要定位权限且系统定位开关开启。`neverForLocation` 表示本 App 不从 BLE 推导位置；如果
业务确实使用 BLE 做位置推导，应删除该标志并按平台政策申请定位权限。

## flutter_blue_plus 兼容边界

当前约束为 `>=1.35.2 <1.40.0`，依赖以下 API：`advName`、`onValueReceived`、
`connectionState`、`BluetoothDevice.fromId`、`connect(autoConnect: true, mtu: null)` 和
`requestMtu`。升级版本时必须逐项核对这些行为。

固件的 UUID 是全零基 128 位：

```text
00000000-0000-0000-0000-00000000fe60
```

不要写成 `Guid('FE60')`；它会展开为 Bluetooth Base UUID，是另一个值。

## 真机冒烟清单

自动测试不能代替手机和硬件验收。使用 `example/`，在 iOS/Android release 构建逐项确认：

- [ ] 能发现 `REAI_VB_`，连接后能读取 chip id、固件和电量；
- [ ] KEY0/KEY1 旋钮每格都有成对 press/release；
- [ ] 按住 Tab 转旋钮再松手，Tab 不会卡住；
- [ ] AI 语音键按住转旋钮不会被误释放；
- [ ] YOLO / PLAN / CHAT 模式事件正确；
- [ ] 前台断开后能恢复；iOS 开启后台模式后锁屏重连有回调；
- [ ] capability + lease 后 FE63 有连续 mSBC，heartbeat 后不中断；
- [ ] stop、LeaseMismatch 或断连后不再向上层投递旧 route 音频。

## 安全

固件 BLE 没有 SDK 内建身份认证或传输层加密。`writeKeyConfig`、休眠、URL 等持久化命令
应在 App 中经过用户确认；不要把扫描到同名前缀当成密码学设备证明。
