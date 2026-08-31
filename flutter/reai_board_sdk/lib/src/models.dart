import 'dart:typed_data';

enum BoardConnectionType { usb, ble }

final class BleDeviceInfo {
  const BleDeviceInfo({
    required this.id,
    required this.name,
    required this.rssi,
  });

  final String id;
  final String name;
  final int rssi;

  @override
  bool operator ==(Object other) =>
      other is BleDeviceInfo &&
      other.id == id &&
      other.name == name &&
      other.rssi == rssi;

  @override
  int get hashCode => Object.hash(id, name, rssi);
}

final class DeviceInfo {
  const DeviceInfo({
    required this.mode,
    required this.macAddress,
    required this.receiverVersion,
    required this.firmwareVersion,
    required this.batteryLevel,
    required this.batteryCharging,
    required this.batteryFull,
    required this.chipId,
    this.connectionType = BoardConnectionType.ble,
  });

  final int mode;
  final String macAddress;
  final String receiverVersion;
  final String firmwareVersion;
  final int batteryLevel;
  final bool batteryCharging;
  final bool batteryFull;
  final String chipId;
  final BoardConnectionType connectionType;
}

enum WorkMode {
  chat(0, 'CHAT'),
  yolo(1, 'YOLO'),
  plan(2, 'PLAN');

  const WorkMode(this.value, this.label);

  final int value;
  final String label;

  static WorkMode? fromValue(int value) => switch (value) {
    0x00 || 0x0C => chat,
    0x01 || 0x0A => yolo,
    0x02 || 0x0B || 0x0F => plan,
    _ => null,
  };
}

final class SleepTimeout {
  const SleepTimeout(this.disconnectedSeconds, this.connectedSeconds);

  final int disconnectedSeconds;
  final int connectedSeconds;

  @override
  bool operator ==(Object other) =>
      other is SleepTimeout &&
      other.disconnectedSeconds == disconnectedSeconds &&
      other.connectedSeconds == connectedSeconds;

  @override
  int get hashCode => Object.hash(disconnectedSeconds, connectedSeconds);
}

final class KeyConfig {
  KeyConfig(Uint8List bytes) : bytes = Uint8List.fromList(bytes) {
    if (bytes.length != 60) {
      throw ArgumentError.value(bytes.length, 'bytes.length', '必须是 60');
    }
  }

  final Uint8List bytes;
}

enum AudioStreamAction {
  stop(0),
  start(1),
  heartbeat(2);

  const AudioStreamAction(this.value);
  final int value;
}

enum AudioStreamScope {
  session(1),
  timeline(2);

  const AudioStreamScope(this.value);
  final int value;
}

enum AudioStreamResult {
  ok(0),
  unsupportedVersion(1),
  busy(2),
  leaseMismatch(3),
  invalidArgument(4),
  transportUnavailable(5);

  const AudioStreamResult(this.value);
  final int value;

  static AudioStreamResult? fromValue(int value) {
    for (final result in values) {
      if (result.value == value) return result;
    }
    return null;
  }
}

final class AudioCapabilities {
  const AudioCapabilities({
    required this.protocolVersion,
    required this.usbVendorHidMsbcV1,
    required this.bleGattMsbcV1,
    required this.streamControlV1,
    required this.packetSequenceV1,
    required this.envelopeVersion,
    required this.usbMaxPayload,
    required this.bleMaxPayload,
    required this.defaultTtlMs,
    required this.maxTtlMs,
  });

  final int protocolVersion;
  final bool usbVendorHidMsbcV1;
  final bool bleGattMsbcV1;
  final bool streamControlV1;
  final bool packetSequenceV1;
  final int envelopeVersion;
  final int usbMaxPayload;
  final int bleMaxPayload;
  final int defaultTtlMs;
  final int maxTtlMs;

  bool get supportsBleGatt =>
      protocolVersion == 1 &&
      bleGattMsbcV1 &&
      streamControlV1 &&
      packetSequenceV1 &&
      envelopeVersion == 1 &&
      bleMaxPayload >= 57;
}

sealed class AudioCapabilityState {
  const AudioCapabilityState();
}

final class AudioCapabilityUnqueried extends AudioCapabilityState {
  const AudioCapabilityUnqueried();

  @override
  bool operator ==(Object other) => other is AudioCapabilityUnqueried;

  @override
  int get hashCode => 0;
}

final class AudioCapabilityUnavailable extends AudioCapabilityState {
  const AudioCapabilityUnavailable();

  @override
  bool operator ==(Object other) => other is AudioCapabilityUnavailable;

  @override
  int get hashCode => 1;
}

final class AudioCapabilityReady extends AudioCapabilityState {
  const AudioCapabilityReady(this.capabilities);

  final AudioCapabilities capabilities;
}

final class AudioStreamState {
  const AudioStreamState({
    required this.result,
    required this.protocolVersion,
    required this.activeBleGatt,
    required this.scope,
    required this.leaseId,
    required this.ttlMs,
  });

  final AudioStreamResult result;
  final int protocolVersion;
  final bool activeBleGatt;
  final AudioStreamScope? scope;
  final int leaseId;
  final int ttlMs;

  bool matchesRequest({
    required AudioStreamAction action,
    required AudioStreamScope scope,
    required int leaseId,
  }) {
    if (result != AudioStreamResult.ok || protocolVersion != 1) return false;
    return switch (action) {
      AudioStreamAction.start || AudioStreamAction.heartbeat =>
        activeBleGatt &&
            this.scope == scope &&
            this.leaseId == leaseId &&
            ttlMs != 0,
      AudioStreamAction.stop =>
        !activeBleGatt && this.scope == null && this.leaseId == 0 && ttlMs == 0,
    };
  }
}

final class EncodedAudioFrame {
  const EncodedAudioFrame({
    required this.payload,
    required this.sequence,
    required this.deviceDiscontinuity,
    this.sequenceGapFrames = 0,
    this.connectionEpoch = 0,
    this.routeEpoch = 0,
  });

  final Uint8List payload;
  final int? sequence;
  final bool deviceDiscontinuity;
  final int sequenceGapFrames;
  final int connectionEpoch;
  final int routeEpoch;

  bool get discontinuity => deviceDiscontinuity || sequenceGapFrames != 0;

  EncodedAudioFrame copyWith({
    int? sequenceGapFrames,
    int? connectionEpoch,
    int? routeEpoch,
  }) => EncodedAudioFrame(
    payload: payload,
    sequence: sequence,
    deviceDiscontinuity: deviceDiscontinuity,
    sequenceGapFrames: sequenceGapFrames ?? this.sequenceGapFrames,
    connectionEpoch: connectionEpoch ?? this.connectionEpoch,
    routeEpoch: routeEpoch ?? this.routeEpoch,
  );
}

final class BoardConfig {
  const BoardConfig({
    this.commandTimeout = const Duration(seconds: 5),
    this.notificationSettleDelay = const Duration(milliseconds: 500),
    this.scanTimeout = const Duration(seconds: 10),
    this.autoReconnect = true,
    this.notifyAppOnlineOnConnect = true,
  });

  final Duration commandTimeout;
  final Duration notificationSettleDelay;
  final Duration scanTimeout;
  final bool autoReconnect;
  final bool notifyAppOnlineOnConnect;
}
