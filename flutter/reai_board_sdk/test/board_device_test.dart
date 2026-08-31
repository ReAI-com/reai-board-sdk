import 'dart:async';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:reai_board_sdk/reai_board_sdk.dart';

class FakeBoardClock implements BoardClock {
  Duration elapsed = Duration.zero;
  final List<(Duration, Completer<void>)> _waiters = [];

  @override
  Duration get now => elapsed;

  @override
  Future<void> delay(Duration duration) {
    final completer = Completer<void>();
    _waiters.add((elapsed + duration, completer));
    return completer.future;
  }

  void advance(Duration duration) {
    elapsed += duration;
    final ready = _waiters.where((waiter) => waiter.$1 <= elapsed).toList();
    _waiters.removeWhere((waiter) => waiter.$1 <= elapsed);
    for (final waiter in ready) {
      waiter.$2.complete();
    }
  }
}

class FakeBoardTransport implements BoardBleTransport {
  final states = StreamController<BoardTransportState>.broadcast();
  final notifications = StreamController<BoardNotification>.broadcast();
  final writes = <List<int>>[];
  int payload = 244;
  bool connected = false;
  int connectCalls = 0;
  final autoConnectValues = <bool>[];

  @override
  Stream<BoardTransportState> get connectionStates => states.stream;

  @override
  Stream<BoardNotification> get notificationStream => notifications.stream;

  @override
  int get maxGattPayload => payload;

  @override
  Future<List<BleDeviceInfo>> scan({Duration? timeout}) async => const [
    BleDeviceInfo(id: 'board-1', name: 'REAI_VB_0729', rssi: -42),
  ];

  @override
  Future<void> connect(BleDeviceInfo device, {bool autoConnect = false}) async {
    connectCalls++;
    autoConnectValues.add(autoConnect);
    connected = true;
    states.add(BoardTransportState.connected);
  }

  @override
  Future<void> disconnect() async {
    connected = false;
    states.add(BoardTransportState.disconnected);
  }

  @override
  Future<void> write(Uint8List data) async {
    writes.add(data.toList());
  }

  void event(List<int> data) {
    notifications.add(BoardNotification.event(Uint8List.fromList(data)));
  }

  void audio(List<int> data) {
    notifications.add(BoardNotification.audio(Uint8List.fromList(data)));
  }

  @override
  Future<void> dispose() async {
    await states.close();
    await notifications.close();
  }
}

Future<void> flush() => Future<void>.delayed(Duration.zero);

void main() {
  late FakeBoardClock clock;
  late FakeBoardTransport transport;
  late BoardDevice device;
  const board = BleDeviceInfo(id: 'board-1', name: 'REAI_VB_0729', rssi: -42);

  setUp(() async {
    clock = FakeBoardClock();
    transport = FakeBoardTransport();
    device = BoardDevice(
      transport: transport,
      clock: clock,
      config: const BoardConfig(
        notificationSettleDelay: Duration.zero,
        commandTimeout: Duration(seconds: 5),
        notifyAppOnlineOnConnect: false,
      ),
    );
    await device.start();
    await device.connect(board);
  });

  tearDown(() async {
    await device.shutdown();
  });

  test('命令严格串行，前一条完成后才写下一条', () async {
    final first = device.getWorkMode();
    final second = device.getSilentRecord();
    await flush();
    expect(transport.writes, [BoardProtocol.getWorkMode()]);

    transport.event([0x12, 0x02, 0xC9, 0x01]);
    expect(await first, WorkMode.yolo);
    await flush();
    expect(transport.writes, [
      BoardProtocol.getWorkMode(),
      BoardProtocol.getSilentRecord(),
    ]);

    transport.event([0x61, 0x02, 0x00, 0x01]);
    expect(await second, isTrue);
  });

  test('pending 0x12 优先作为响应，不重复发模式推送', () async {
    final events = <BoardEvent>[];
    final sub = device.events.listen(events.add);
    final future = device.getWorkMode();
    await flush();
    transport.event([0x12, 0x02, 0xC9, 0x02]);
    expect(await future, WorkMode.plan);
    await flush();
    expect(events.whereType<ModeChangeEvent>(), isEmpty);
    await sub.cancel();
  });

  test('命令超时会完成 Future 并允许后续命令继续', () async {
    final first = device.getWorkMode();
    await flush();
    clock.advance(const Duration(seconds: 5));
    await expectLater(first, throwsA(isA<BoardTimeoutException>()));

    final second = device.getSilentRecord();
    await flush();
    transport.event([0x61, 0x02, 0x00, 0x00]);
    expect(await second, isFalse);
  });

  test('低 MTU 不阻断输入事件，但受限命令给出 typed error', () async {
    transport.payload = 20;
    final events = <BoardEvent>[];
    final sub = device.events.listen(events.add);
    transport.event([0x0C, 0x02, 0x01, 0x0F]);
    await flush();
    expect(events.whereType<KeyPressEvent>().single.keyIndex, 3);
    await expectLater(
      device.readDeviceInfo(),
      throwsA(isA<BoardMtuException>()),
    );
    await sub.cancel();
  });

  test('断连时 pending 失败且 capability 回到 unqueried', () async {
    final pending = device.getWorkMode();
    await flush();
    transport.states.add(BoardTransportState.disconnected);
    await expectLater(pending, throwsA(isA<BoardDisconnectedException>()));
    expect(device.audioCapabilityState, const AudioCapabilityUnqueried());
  });

  test('capability 查询遇到断连不会把新连接状态回写成 unavailable', () async {
    final pending = device.queryAudioCapabilities();
    await flush();
    transport.states.add(BoardTransportState.disconnected);
    await expectLater(pending, throwsA(isA<BoardDisconnectedException>()));
    expect(device.audioCapabilityState, const AudioCapabilityUnqueried());
  });

  test('意外断连按退避重连已知 peripheral id', () async {
    transport.states.add(BoardTransportState.disconnected);
    await flush();
    expect(transport.connectCalls, 1);

    clock.advance(const Duration(seconds: 1));
    await flush();
    await flush();
    expect(transport.connectCalls, 2);
    expect(transport.autoConnectValues, [false, true]);
    expect(device.isConnected, isTrue);
  });

  test('精确 lease ack 才开放版本化音频，并计算丢帧', () async {
    final frames = <EncodedAudioFrame>[];
    final sub = device.audioFrames.listen(frames.add);

    final capabilities = device.queryAudioCapabilities();
    await flush();
    transport.event([
      0x6E,
      13,
      0,
      1,
      15,
      0,
      0,
      0,
      1,
      57,
      57,
      0x88,
      0x13,
      0x88,
      0x13,
    ]);
    expect((await capabilities).supportsBleGatt, isTrue);

    final start = device.controlAudioStream(
      action: AudioStreamAction.start,
      scope: AudioStreamScope.session,
      leaseId: 0x12345678,
      ttlMs: 5000,
    );
    await flush();
    transport.event([0x6F, 10, 0, 1, 2, 1, 0x78, 0x56, 0x34, 0x12, 0x88, 0x13]);
    await start;

    transport.audio([1, 1, 1, 0, 57, ...List<int>.filled(57, 0xAD)]);
    transport.audio([1, 1, 4, 0, 57, ...List<int>.filled(57, 0xBC)]);
    await flush();
    expect(frames, hasLength(2));
    expect(frames.first.sequence, 1);
    expect(frames.last.sequenceGapFrames, 2);
    expect(frames.last.discontinuity, isTrue);
    expect(frames.last.connectionEpoch, 1);

    final stop = device.controlAudioStream(
      action: AudioStreamAction.stop,
      scope: AudioStreamScope.session,
      leaseId: 0x12345678,
      ttlMs: 0,
    );
    await flush();
    transport.event([0x6F, 10, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    await stop;
    transport.audio([1, 1, 5, 0, 57, ...List<int>.filled(57, 0xEF)]);
    await flush();
    expect(frames, hasLength(2));
    await sub.cancel();
  });

  test('lease ack owner 不匹配时拒绝并保持音频路由关闭', () async {
    final frames = <EncodedAudioFrame>[];
    final sub = device.audioFrames.listen(frames.add);
    final capabilities = device.queryAudioCapabilities();
    await flush();
    transport.event([
      0x6E,
      13,
      0,
      1,
      15,
      0,
      0,
      0,
      1,
      57,
      57,
      0x88,
      0x13,
      0x88,
      0x13,
    ]);
    await capabilities;

    final start = device.controlAudioStream(
      action: AudioStreamAction.start,
      scope: AudioStreamScope.session,
      leaseId: 0x12345678,
      ttlMs: 5000,
    );
    await flush();
    transport.event([0x6F, 10, 0, 1, 2, 2, 0x78, 0x56, 0x34, 0x12, 0x88, 0x13]);
    await expectLater(start, throwsA(isA<BoardProtocolException>()));
    transport.audio([1, 1, 1, 0, 57, ...List<int>.filled(57, 0xAD)]);
    await flush();
    expect(frames, isEmpty);
    await sub.cancel();
  });

  test('旧固件逃生口只转发显式开启后的 legacy session 音频', () async {
    final frames = <EncodedAudioFrame>[];
    final sub = device.audioFrames.listen(frames.add);
    transport.audio([1, 57, ...List<int>.filled(57, 0xAD)]);
    await flush();
    expect(frames, isEmpty);

    device.startLegacySessionAudio();
    transport.audio([1, 57, ...List<int>.filled(57, 0xAD)]);
    await flush();
    expect(frames.single.sequence, isNull);
    expect(frames.single.payload, hasLength(57));
    device.stopLegacySessionAudio();
    await sub.cancel();
  });
}
