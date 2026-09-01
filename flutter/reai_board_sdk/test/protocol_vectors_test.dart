import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:reai_board_sdk/reai_board_sdk.dart';

Map<String, dynamic> loadFixture() {
  final file = File('../../tests/fixtures/flutter_protocol_vectors.json');
  expect(
    file.existsSync(),
    isTrue,
    reason: '共享 fixture 不存在：${file.absolute.path}',
  );
  return jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
}

List<int> vector(Map<String, dynamic> fixture, String group, String name) =>
    ((fixture[group] as Map<String, dynamic>)[name] as List<dynamic>)
        .cast<int>();

void main() {
  late Map<String, dynamic> fixture;

  setUpAll(() {
    fixture = loadFixture();
  });

  test('固件 UUID 只能使用全零基 128 位格式', () {
    final uuids = fixture['uuids'] as Map<String, dynamic>;
    expect(BoardGatt.devicePrefix, fixture['device_prefix']);
    expect(BoardGatt.serviceUuid, uuids['service']);
    expect(BoardGatt.commandUuid, uuids['command']);
    expect(BoardGatt.eventUuid, uuids['event']);
    expect(BoardGatt.audioUuid, uuids['audio']);
    expect(
      BoardGatt.serviceUuid,
      isNot('0000fe60-0000-1000-8000-00805f9b34fb'),
    );
  });

  test('移动端命令逐字节对齐 Rust golden', () {
    final keyData = Uint8List.fromList(
      List<int>.generate(60, (index) => index),
    );
    expect(
      BoardProtocol.getDeviceInfo(),
      vector(fixture, 'commands', 'get_device_info'),
    );
    expect(
      BoardProtocol.getKeyConfig(),
      vector(fixture, 'commands', 'get_key_config'),
    );
    expect(
      BoardProtocol.setKeyConfig(keyData),
      vector(fixture, 'commands', 'set_key_config'),
    );
    expect(
      BoardProtocol.getWorkMode(),
      vector(fixture, 'commands', 'get_work_mode'),
    );
    expect(
      BoardProtocol.getSilentRecord(),
      vector(fixture, 'commands', 'get_silent_record'),
    );
    expect(
      BoardProtocol.setSilentRecord(true),
      vector(fixture, 'commands', 'set_silent_record_true'),
    );
    expect(
      BoardProtocol.getSleepTimeout(),
      vector(fixture, 'commands', 'get_sleep_timeout'),
    );
    expect(
      BoardProtocol.setSleepTimeout(const SleepTimeout(60, 600)),
      vector(fixture, 'commands', 'set_sleep_timeout'),
    );
    expect(
      BoardProtocol.notifyAppOnline(true),
      vector(fixture, 'commands', 'notify_app_online'),
    );
    expect(
      BoardProtocol.getAppOnline(),
      vector(fixture, 'commands', 'get_app_online'),
    );
    expect(
      BoardProtocol.getOpenUrl(),
      vector(fixture, 'commands', 'get_open_url'),
    );
    expect(
      BoardProtocol.getAudioCapabilities(),
      vector(fixture, 'commands', 'get_audio_capabilities'),
    );
    expect(
      BoardProtocol.audioStreamControl(
        action: AudioStreamAction.start,
        scope: AudioStreamScope.session,
        leaseId: 0x12345678,
        ttlMs: 5000,
      ),
      vector(fixture, 'commands', 'start_audio'),
    );
  });

  test('解析设备信息、工作模式和常用设置响应', () {
    final info = BoardProtocol.parseDeviceInfo(
      vector(fixture, 'responses', 'device_info'),
    );
    expect(info.connectionType, BoardConnectionType.ble);
    expect(info.macAddress, 'AA:BB:CC:DD:EE:FF');
    expect(info.firmwareVersion, '1.55');
    expect(info.batteryLevel, 73);
    expect(info.chipId, '1CE60729');

    expect(
      BoardProtocol.parseWorkMode(
        vector(fixture, 'responses', 'work_mode_yolo'),
      ),
      WorkMode.yolo,
    );
    expect(
      BoardProtocol.parseBooleanResponse(
        vector(fixture, 'responses', 'silent_record_on'),
        BoardCommand.getSilentRecord,
      ),
      isTrue,
    );
    expect(
      BoardProtocol.parseSleepTimeout(
        vector(fixture, 'responses', 'sleep_timeout'),
        BoardCommand.getSleepTimeout,
      ),
      const SleepTimeout(60, 600),
    );
    expect(
      BoardProtocol.parseOpenUrl(vector(fixture, 'responses', 'open_url')),
      'https://x',
    );
  });

  test('解析 capability 和精确匹配的音频 lease ack', () {
    final capabilities = BoardProtocol.parseAudioCapabilities(
      vector(fixture, 'responses', 'audio_capabilities'),
    );
    expect(capabilities.supportsBleGatt, isTrue);
    expect(capabilities.bleMaxPayload, 57);

    final state = BoardProtocol.parseAudioStreamState(
      vector(fixture, 'responses', 'audio_started'),
    );
    expect(
      state.matchesRequest(
        action: AudioStreamAction.start,
        scope: AudioStreamScope.session,
        leaseId: 0x12345678,
      ),
      isTrue,
    );
    expect(
      state.matchesRequest(
        action: AudioStreamAction.start,
        scope: AudioStreamScope.timeline,
        leaseId: 0x12345678,
      ),
      isFalse,
    );
  });

  test('版本化音频包校验长度并表达 sequence', () {
    final header = vector(fixture, 'audio', 'versioned_header');
    final packet = Uint8List.fromList([
      ...header,
      ...List<int>.filled(57, 0xAD),
    ]);
    final frame = BoardProtocol.parseAudioPacket(packet);
    expect(frame.sequence, 0x1234);
    expect(frame.deviceDiscontinuity, isTrue);
    expect(frame.payload, hasLength(57));
    expect(
      () => BoardProtocol.parseAudioPacket(
        Uint8List.fromList([...header, 1, 2, 3]),
      ),
      throwsA(isA<BoardProtocolException>()),
    );
  });
}
