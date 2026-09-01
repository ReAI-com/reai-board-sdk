import 'dart:typed_data';

import 'constants.dart';
import 'models.dart';

enum BoardTransportState { connecting, connected, disconnected }

enum BoardNotificationType { event, audio }

final class BoardNotification {
  const BoardNotification(this.type, this.data);

  factory BoardNotification.event(Uint8List data) =>
      BoardNotification(BoardNotificationType.event, data);

  factory BoardNotification.audio(Uint8List data) =>
      BoardNotification(BoardNotificationType.audio, data);

  final BoardNotificationType type;
  final Uint8List data;
}

/// 手机 BLE I/O 端口。协议和状态机只依赖此接口。
abstract interface class BoardBleTransport {
  Stream<BoardTransportState> get connectionStates;

  Stream<BoardNotification> get notificationStream;

  /// 当前一次扫描的增量快照。每发现或更新设备时立即推送，不等待扫描超时。
  Stream<List<BleDeviceInfo>> get scanResults;

  /// 当前 ATT MTU 可承载的 GATT 数据长度，写命令和通知都受此上限约束。
  int get maxGattPayload;

  /// GATT 有效载荷变化流；iOS 会在连接后自动协商 MTU，因此不能只读取初始值。
  Stream<int> get maxGattPayloads;

  Future<List<BleDeviceInfo>> scan({Duration? timeout});

  Future<void> connect(BleDeviceInfo device, {bool autoConnect = false});

  Future<void> write(Uint8List data);

  Future<void> disconnect();

  Future<void> dispose();
}

/// 可选的系统已连接设备查询能力。拆成独立接口以保持自定义 transport 兼容。
abstract interface class BoardSystemDeviceTransport {
  Future<List<BleDeviceInfo>> systemDevices({
    String serviceUuid = BoardGatt.serviceUuid,
  });
}
