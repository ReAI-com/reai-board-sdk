import 'dart:convert';
import 'dart:typed_data';

import 'constants.dart';
import 'errors.dart';
import 'models.dart';

abstract final class BoardProtocol {
  static const _audioProtocolVersion = 1;
  static const _audioFlagData = 0x01;
  static const _audioFlagDiscontinuity = 0x02;
  static const _msbcFrameSize = 57;

  static Uint8List _packet(
    int command,
    List<int> payload, {
    int? declaredLength,
  }) => Uint8List.fromList([
    command,
    declaredLength ?? payload.length,
    ...payload,
  ]);

  static Uint8List getDeviceInfo() =>
      _packet(BoardCommand.getDeviceInfo, [0, 0]);

  static Uint8List getKeyConfig() =>
      _packet(BoardCommand.getKeyConfig, const []);

  static Uint8List setKeyConfig(Uint8List bytes) {
    if (bytes.length != 60) {
      throw ArgumentError.value(bytes.length, 'bytes.length', '按键配置必须是 60 字节');
    }
    return _packet(BoardCommand.setKeyConfig, bytes);
  }

  static Uint8List getWorkMode() =>
      _packet(BoardCommand.status, [BoardCommand.workModeData, 0, 0, 0]);

  static Uint8List getSilentRecord() =>
      _packet(BoardCommand.getSilentRecord, const []);

  static Uint8List setSilentRecord(bool enabled) =>
      _packet(BoardCommand.setSilentRecord, [enabled ? 1 : 0]);

  static Uint8List getSleepTimeout() =>
      _packet(BoardCommand.getSleepTimeout, const []);

  static Uint8List setSleepTimeout(SleepTimeout timeout) => _packet(
    BoardCommand.setSleepTimeout,
    [..._u16(timeout.disconnectedSeconds), ..._u16(timeout.connectedSeconds)],
  );

  static Uint8List notifyAppOnline(bool online) =>
      _packet(BoardCommand.appOnlineNotify, [online ? 1 : 0]);

  static Uint8List getAppOnline() =>
      _packet(BoardCommand.getAppOnline, const []);

  static Uint8List getOpenUrl() => _packet(BoardCommand.getOpenUrl, const []);

  /// 对齐 Rust 现有 64 字节 HID packet：声明 64，GATT 实际最多携带 61 字节。
  static Uint8List setOpenUrl(String url) {
    final bytes = utf8.encode(url);
    final payload = Uint8List(61);
    payload.setRange(0, bytes.length.clamp(0, 60), bytes.take(60));
    return _packet(BoardCommand.setOpenUrl, payload, declaredLength: 64);
  }

  static Uint8List getAudioCapabilities() =>
      _packet(BoardCommand.getAudioCapabilities, const []);

  static Uint8List audioStreamControl({
    required AudioStreamAction action,
    required AudioStreamScope scope,
    required int leaseId,
    required int ttlMs,
  }) {
    if (leaseId == 0) throw ArgumentError.value(leaseId, 'leaseId', '不能为 0');
    return _packet(BoardCommand.audioStreamControl, [
      action.value,
      0x02,
      scope.value,
      _audioProtocolVersion,
      ..._u32(leaseId),
      ..._u16(ttlMs),
    ]);
  }

  static DeviceInfo parseDeviceInfo(List<int> response) {
    _expectCommand(response, BoardCommand.getDeviceInfo, minimumBytes: 23);
    if (response[2] != 0) {
      throw BoardProtocolException('读取设备信息失败：result=${response[2]}');
    }
    final payload = response.sublist(3);
    if (payload.length < 20) {
      throw const BoardProtocolException('设备信息 payload 长度不足');
    }
    String hexByte(int value) =>
        value.toRadixString(16).padLeft(2, '0').toUpperCase();
    return DeviceInfo(
      mode: payload[0],
      macAddress: payload.sublist(1, 7).map(hexByte).join(':'),
      receiverVersion: '${payload[8]}.${payload[7]}',
      firmwareVersion: '${payload[10]}.${payload[9]}',
      batteryCharging: payload[13] != 0,
      batteryLevel: payload[14],
      batteryFull: payload[15] != 0,
      chipId: payload.sublist(16, 20).map(hexByte).join(),
    );
  }

  static KeyConfig parseKeyConfig(List<int> response) {
    _expectCommand(response, BoardCommand.getKeyConfig, minimumBytes: 63);
    if (response[2] != 0) {
      throw BoardProtocolException('读取按键配置失败：result=${response[2]}');
    }
    return KeyConfig(Uint8List.fromList(response.sublist(3, 63)));
  }

  static void parseSuccess(List<int> response, int command) {
    _expectCommand(response, command, minimumBytes: 3);
    if (response[2] != 0) {
      throw BoardProtocolException(
        '命令 0x${command.toRadixString(16)} 失败：${response[2]}',
      );
    }
  }

  static WorkMode parseWorkMode(List<int> response) {
    _expectCommand(response, BoardCommand.status, minimumBytes: 4);
    if (response[2] != BoardCommand.workModeData) {
      throw const BoardProtocolException('不是工作模式响应');
    }
    final mode = WorkMode.fromValue(response[3]);
    if (mode == null) throw BoardProtocolException('未知工作模式：${response[3]}');
    return mode;
  }

  static bool parseBooleanResponse(List<int> response, int command) {
    _expectCommand(response, command, minimumBytes: 4);
    if (response[2] != 0) {
      throw BoardProtocolException('布尔命令失败：result=${response[2]}');
    }
    return response[3] != 0;
  }

  static SleepTimeout parseSleepTimeout(List<int> response, int command) {
    _expectCommand(response, command, minimumBytes: 7);
    if (response[2] != 0) {
      throw BoardProtocolException('休眠配置命令失败：result=${response[2]}');
    }
    return SleepTimeout(_readU16(response, 3), _readU16(response, 5));
  }

  static String parseOpenUrl(List<int> response) {
    _expectCommand(response, BoardCommand.getOpenUrl, minimumBytes: 4);
    if (response[2] != 0) {
      throw BoardProtocolException('读取 URL 失败：result=${response[2]}');
    }
    final bytes = response.sublist(3);
    final end = bytes.indexOf(0);
    return utf8.decode(
      end < 0 ? bytes : bytes.sublist(0, end),
      allowMalformed: true,
    );
  }

  static AudioCapabilities parseAudioCapabilities(List<int> response) {
    _expectCommand(
      response,
      BoardCommand.getAudioCapabilities,
      minimumBytes: 15,
    );
    if (response[1] != 13 || response[2] != 0) {
      throw const BoardUnsupportedException('固件不支持版本化板载音频 capability');
    }
    final bits = _readU32(response, 4);
    return AudioCapabilities(
      protocolVersion: response[3],
      usbVendorHidMsbcV1: bits & 1 != 0,
      bleGattMsbcV1: bits & 2 != 0,
      streamControlV1: bits & 4 != 0,
      packetSequenceV1: bits & 8 != 0,
      envelopeVersion: response[8],
      usbMaxPayload: response[9],
      bleMaxPayload: response[10],
      defaultTtlMs: _readU16(response, 11),
      maxTtlMs: _readU16(response, 13),
    );
  }

  static AudioStreamState parseAudioStreamState(List<int> response) {
    _expectCommand(response, BoardCommand.audioStreamControl, minimumBytes: 12);
    if (response[1] != 10) {
      throw const BoardProtocolException('音频 stream ack 长度无效');
    }
    final result = AudioStreamResult.fromValue(response[2]);
    if (result == null) {
      throw BoardProtocolException('未知 stream result：${response[2]}');
    }
    final transport = response[4];
    if (transport != 0 && transport != 2) {
      throw BoardProtocolException('移动端不支持音频 transport：$transport');
    }
    final scope = switch (response[5]) {
      0 => null,
      1 => AudioStreamScope.session,
      2 => AudioStreamScope.timeline,
      final value => throw BoardProtocolException('未知音频 scope：$value'),
    };
    return AudioStreamState(
      result: result,
      protocolVersion: response[3],
      activeBleGatt: transport == 2,
      scope: scope,
      leaseId: _readU32(response, 6),
      ttlMs: _readU16(response, 10),
    );
  }

  /// 音频 envelope 不能靠首字节自动判断：legacy data flag 与 v1 version 都是 1。
  /// 调用方必须根据已经建立的音频路由显式选择格式。
  static EncodedAudioFrame parseAudioPacket(
    Uint8List packet, {
    bool legacy = false,
  }) {
    if (!legacy) {
      if (packet.length < 5 || packet[0] != _audioProtocolVersion) {
        throw const BoardProtocolException('版本化音频包无效');
      }
      final flags = packet[1];
      if (flags & _audioFlagData == 0 ||
          flags & ~(_audioFlagData | _audioFlagDiscontinuity) != 0) {
        throw const BoardProtocolException('版本化音频 flags 无效');
      }
      final payloadLength = packet[4];
      if (payloadLength == 0 ||
          payloadLength % _msbcFrameSize != 0 ||
          packet.length != 5 + payloadLength) {
        throw const BoardProtocolException('版本化音频长度无效');
      }
      return EncodedAudioFrame(
        payload: Uint8List.sublistView(packet, 5),
        sequence: packet[2] | (packet[3] << 8),
        deviceDiscontinuity: flags & _audioFlagDiscontinuity != 0,
      );
    }

    if (packet.length < 2 || packet[0] != 1) {
      throw const BoardProtocolException('旧版音频包无效');
    }
    final actualLength = packet[1].clamp(0, packet.length - 2);
    if (actualLength == 0) {
      throw const BoardProtocolException('旧版音频 payload 为空');
    }
    return EncodedAudioFrame(
      payload: Uint8List.sublistView(packet, 2, 2 + actualLength),
      sequence: null,
      deviceDiscontinuity: false,
    );
  }

  static ({int command, Uint8List payload}) parsePacket(Uint8List packet) {
    if (packet.length < 2) throw const BoardProtocolException('GATT 包过短');
    final length = packet[1];
    if (packet.length < 2 + length) {
      throw BoardProtocolException(
        'GATT 包声明 $length 字节，实际只有 ${packet.length - 2}',
      );
    }
    return (
      command: packet[0],
      payload: Uint8List.sublistView(packet, 2, 2 + length),
    );
  }

  static void _expectCommand(
    List<int> response,
    int command, {
    required int minimumBytes,
  }) {
    if (response.length < minimumBytes) {
      throw BoardProtocolException('响应长度不足：${response.length} < $minimumBytes');
    }
    if (response[0] != command) {
      throw BoardProtocolException('响应命令不匹配：${response[0]} != $command');
    }
  }

  static List<int> _u16(int value) => [value & 0xFF, (value >> 8) & 0xFF];
  static List<int> _u32(int value) => [
    value & 0xFF,
    (value >> 8) & 0xFF,
    (value >> 16) & 0xFF,
    (value >> 24) & 0xFF,
  ];
  static int _readU16(List<int> bytes, int offset) =>
      bytes[offset] | (bytes[offset + 1] << 8);
  static int _readU32(List<int> bytes, int offset) =>
      bytes[offset] |
      (bytes[offset + 1] << 8) |
      (bytes[offset + 2] << 16) |
      (bytes[offset + 3] << 24);
}
