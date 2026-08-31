import 'dart:typed_data';

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

  /// 当前 ATT MTU 可承载的 GATT 数据长度，写命令和通知都受此上限约束。
  int get maxGattPayload;

  Future<List<BleDeviceInfo>> scan({Duration? timeout});

  Future<void> connect(BleDeviceInfo device, {bool autoConnect = false});

  Future<void> write(Uint8List data);

  Future<void> disconnect();

  Future<void> dispose();
}
