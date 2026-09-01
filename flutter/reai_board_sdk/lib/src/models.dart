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

abstract final class BoardKeyClass {
  static const media = 0x0A;
  static const keyboard = 0x0B;
  static const aiVoice = 0x0E;
  static const disabled = 0xFF;
}

enum BoardKeyGroup {
  knob('旋钮'),
  function('功能键'),
  mode('模式拨杆');

  const BoardKeyGroup(this.label);
  final String label;
}

final class KeyBinding {
  const KeyBinding({required this.keyClass, required this.keyValue})
    : assert(keyClass >= 0 && keyClass <= 0xFF),
      assert(keyValue >= 0 && keyValue <= 0xFFFF);

  final int keyClass;
  final int keyValue;

  String get description {
    if (keyClass == BoardKeyClass.media) {
      return _mediaBindingNames[keyValue] ??
          (keyValue >= 0x0F10 && keyValue <= 0x0FFF
              ? '脚本触发 ${_hex16(keyValue)}（需桌面客户端）'
              : _hex16(keyValue));
    }
    if (keyClass == BoardKeyClass.keyboard) {
      final modifier = (keyValue >> 8) & 0xFF;
      final usage = keyValue & 0xFF;
      final parts = <String>[
        for (final entry in _modifierNames.entries)
          if ((modifier & entry.key) != 0) entry.value,
      ];
      final usageLabel = _keyboardUsageLabel(usage);
      if (usageLabel != null) parts.add(usageLabel);
      return parts.isEmpty ? _hex16(keyValue) : parts.join(' + ');
    }
    if (keyClass == BoardKeyClass.aiVoice) return 'AI 语音（固件标记）';
    if (keyClass == BoardKeyClass.disabled) return '禁用';
    if (keyClass == 0 && keyValue == 0) return '未配置';
    return '未知 ${_hex16(keyValue)}';
  }

  @override
  bool operator ==(Object other) =>
      other is KeyBinding &&
      other.keyClass == keyClass &&
      other.keyValue == keyValue;

  @override
  int get hashCode => Object.hash(keyClass, keyValue);
}

enum BoardPhysicalKey {
  knobLeft(BoardKeyGroup.knob, '◀', '旋钮左旋', 0x0F07),
  knobRight(BoardKeyGroup.knob, '▶', '旋钮右旋', 0x0F08),
  knobPress(BoardKeyGroup.knob, '⏻', '旋钮按压', 0x0F09),
  tab(BoardKeyGroup.function, 'TAB', 'Tab 键', 0x0F01),
  createNew(BoardKeyGroup.function, 'NEW', 'New 键', 0x0F02),
  escape(BoardKeyGroup.function, 'ESC', 'Esc 键', 0x0F03),
  aiVoice(BoardKeyGroup.function, 'AI', 'AI 语音键', 0x0F04),
  action(BoardKeyGroup.function, 'ACT', 'Action 键', 0x0F05),
  enter(BoardKeyGroup.function, '↵', 'Enter 键', 0x0F06),
  yolo(BoardKeyGroup.mode, 'YOLO', 'YOLO 拨杆', 0x0F0A),
  plan(BoardKeyGroup.mode, 'PLAN', 'PLAN 拨杆', 0x0F0B),
  chat(BoardKeyGroup.mode, 'CHAT', 'CHAT 拨杆', 0x0F0C);

  const BoardPhysicalKey(
    this.group,
    this.label,
    this.name,
    this.defaultKeyValue,
  );

  final BoardKeyGroup group;
  final String label;
  final String name;
  final int defaultKeyValue;

  KeyBinding get defaultBinding =>
      KeyBinding(keyClass: BoardKeyClass.media, keyValue: defaultKeyValue);
}

final class KeyConfig {
  static const keyCount = 20;
  static const activeKeyCount = 12;
  static const bytesPerKey = 3;
  static const byteLength = keyCount * bytesPerKey;

  KeyConfig(Uint8List bytes) : bytes = Uint8List.fromList(bytes) {
    if (bytes.length != byteLength) {
      throw ArgumentError.value(
        bytes.length,
        'bytes.length',
        '必须是 $byteLength',
      );
    }
  }

  final Uint8List bytes;

  factory KeyConfig.fromActiveBindings(List<KeyBinding> bindings) {
    if (bindings.length != activeKeyCount) {
      throw ArgumentError.value(
        bindings.length,
        'bindings.length',
        '必须是 $activeKeyCount',
      );
    }
    final bytes = Uint8List(byteLength);
    for (var index = 0; index < activeKeyCount; index++) {
      final binding = bindings[index];
      final offset = index * bytesPerKey;
      bytes[offset] = binding.keyClass;
      bytes[offset + 1] = binding.keyValue & 0xFF;
      bytes[offset + 2] = (binding.keyValue >> 8) & 0xFF;
    }
    return KeyConfig(bytes);
  }

  factory KeyConfig.factoryDefaults() => KeyConfig.fromActiveBindings([
    for (final key in BoardPhysicalKey.values) key.defaultBinding,
  ]);

  List<KeyBinding> get bindings => List<KeyBinding>.unmodifiable([
    for (var index = 0; index < keyCount; index++)
      KeyBinding(
        keyClass: bytes[index * bytesPerKey],
        keyValue:
            bytes[index * bytesPerKey + 1] |
            (bytes[index * bytesPerKey + 2] << 8),
      ),
  ]);

  List<KeyBinding> get activeBindings =>
      List<KeyBinding>.unmodifiable(bindings.take(activeKeyCount));

  KeyConfig copyWithActiveBinding(int index, KeyBinding binding) {
    if (index < 0 || index >= activeKeyCount) {
      throw RangeError.range(index, 0, activeKeyCount - 1, 'index');
    }
    final updated = Uint8List.fromList(bytes);
    final offset = index * bytesPerKey;
    updated[offset] = binding.keyClass;
    updated[offset + 1] = binding.keyValue & 0xFF;
    updated[offset + 2] = (binding.keyValue >> 8) & 0xFF;
    return KeyConfig(updated);
  }
}

const _mediaBindingNames = <int, String>{
  0x0F01: 'Tab（应用）',
  0x0F02: 'New',
  0x0F03: 'Esc（应用）',
  0x0F04: 'AI 语音',
  0x0F05: 'Action',
  0x0F06: 'Enter（应用）',
  0x0F07: '音量-',
  0x0F08: '音量+',
  0x0F09: '静音',
  0x0F0A: 'YOLO 模式',
  0x0F0B: 'PLAN 模式',
  0x0F0C: 'CHAT 模式',
};

const _modifierNames = <int, String>{
  0x01: 'Ctrl L',
  0x02: 'Shift L',
  0x04: 'Alt L',
  0x08: 'Cmd L',
  0x10: 'Ctrl R',
  0x20: 'Shift R',
  0x40: 'Alt R',
  0x80: 'Cmd R',
};

String _hex16(int value) =>
    '0x${value.toRadixString(16).toUpperCase().padLeft(4, '0')}';

String? _keyboardUsageLabel(int usage) {
  if (usage == 0) return null;
  if (usage >= 0x04 && usage <= 0x1D) {
    return String.fromCharCode('A'.codeUnitAt(0) + usage - 0x04);
  }
  if (usage >= 0x1E && usage <= 0x26) return '${usage - 0x1D}';
  if (usage == 0x27) return '0';
  return const <int, String>{
        0x28: 'Enter',
        0x29: 'Esc',
        0x2A: 'Backspace',
        0x2B: 'Tab',
        0x2C: 'Space',
        0x4F: '→',
        0x50: '←',
        0x51: '↓',
        0x52: '↑',
      }[usage] ??
      'HID 0x${usage.toRadixString(16).toUpperCase().padLeft(2, '0')}';
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
    this.mtuNegotiationTimeout = const Duration(seconds: 5),
    this.notificationSettleDelay = const Duration(milliseconds: 500),
    this.scanTimeout = const Duration(seconds: 10),
    this.autoReconnect = true,
    this.notifyAppOnlineOnConnect = true,
  });

  final Duration commandTimeout;
  final Duration mtuNegotiationTimeout;
  final Duration notificationSettleDelay;
  final Duration scanTimeout;
  final bool autoReconnect;
  final bool notifyAppOnlineOnConnect;
}
