import 'models.dart';

sealed class BoardEvent {
  const BoardEvent();
}

final class ConnectionEvent extends BoardEvent {
  const ConnectionEvent({required this.connected, this.reason});

  final bool connected;
  final String? reason;
}

enum ReconnectState {
  idle,
  waitingForDevice,
  scanning,
  connecting,
  connected,
  suppressed,
}

final class ReconnectEvent extends BoardEvent {
  const ReconnectEvent(this.state, {this.attempt, this.message});

  final ReconnectState state;
  final int? attempt;
  final String? message;
}

enum KeySource { config, consumer, gatt }

final class KeyPressEvent extends BoardEvent {
  const KeyPressEvent({
    required this.keyIndex,
    required this.keyName,
    required this.keyValue,
    required this.pressed,
    this.source = KeySource.gatt,
  });

  final int keyIndex;
  final String keyName;
  final int keyValue;
  final bool pressed;
  final KeySource source;
}

final class ComboKeyEvent extends BoardEvent {
  const ComboKeyEvent(this.keys);

  final List<int> keys;
}

final class AiVoiceKeyEvent extends BoardEvent {
  const AiVoiceKeyEvent(this.pressed);

  final bool pressed;
}

enum ModeSource { dial, connection }

final class ModeChangeEvent extends BoardEvent {
  const ModeChangeEvent(this.mode, {this.source = ModeSource.dial});

  final WorkMode mode;
  final ModeSource source;
}

final class DeviceInfoEvent extends BoardEvent {
  const DeviceInfoEvent(this.info);

  final DeviceInfo info;
}

final class BoardErrorEvent extends BoardEvent {
  const BoardErrorEvent(this.message, {this.recoverable = true});

  final String message;
  final bool recoverable;
}

final class BoardCapabilityWarningEvent extends BoardEvent {
  const BoardCapabilityWarningEvent(this.message);

  final String message;
}
