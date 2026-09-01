import 'dart:async';
import 'dart:typed_data';

import 'clock.dart';
import 'constants.dart';
import 'errors.dart';
import 'events.dart';
import 'key_state.dart';
import 'models.dart';
import 'protocol.dart';
import 'transport.dart';

final class _PendingResponse {
  _PendingResponse(this.command, this.completer);

  final int command;
  final Completer<Uint8List> completer;
}

/// Flutter 移动端门面。公开语义与 Rust `BoardDevice` 对齐，底层只使用 BLE。
final class BoardDevice {
  BoardDevice({
    required BoardBleTransport transport,
    BoardConfig config = const BoardConfig(),
    BoardClock? clock,
  }) : _transport = transport,
       _config = config,
       _clock = clock ?? SystemBoardClock();

  final BoardBleTransport _transport;
  final BoardConfig _config;
  final BoardClock _clock;
  final _events = StreamController<BoardEvent>.broadcast();
  final _audioFrames = StreamController<EncodedAudioFrame>.broadcast();
  final _input = BoardInputInterpreter();

  StreamSubscription<BoardTransportState>? _stateSubscription;
  StreamSubscription<BoardNotification>? _notificationSubscription;
  Future<void> _commandTail = Future<void>.value();
  _PendingResponse? _pending;
  BleDeviceInfo? _target;
  bool _started = false;
  bool _connected = false;
  bool _userDisconnect = false;
  bool _disposed = false;
  int _reconnectGeneration = 0;
  int _connectionEpoch = 0;
  int _routeEpoch = 0;
  bool _audioEnabled = false;
  bool _legacyAudio = false;
  int? _lastAudioSequence;
  AudioCapabilityState _audioCapabilityState = const AudioCapabilityUnqueried();

  Stream<BoardEvent> get events => _events.stream;

  Stream<EncodedAudioFrame> get audioFrames => _audioFrames.stream;

  /// 扫描中的设备列表快照；UI 可据此边扫描边展示。
  Stream<List<BleDeviceInfo>> get scanResults => _transport.scanResults;

  int get maxGattPayload => _transport.maxGattPayload;

  Stream<int> get maxGattPayloads => _transport.maxGattPayloads;

  bool get isConnected => _connected;

  int get connectionEpoch => _connectionEpoch;

  AudioCapabilityState get audioCapabilityState => _audioCapabilityState;

  Future<void> start() async {
    if (_disposed) throw const BoardDisconnectedException('BoardDevice 已释放');
    if (_started) return;
    _started = true;
    _stateSubscription = _transport.connectionStates.listen(_onTransportState);
    _notificationSubscription = _transport.notificationStream.listen(
      _onNotification,
      onError: (Object error) {
        _events.add(BoardErrorEvent('BLE 通知流失败：$error'));
      },
    );
  }

  Future<List<BleDeviceInfo>> scan({Duration? timeout}) async {
    await start();
    return _transport.scan(timeout: timeout ?? _config.scanTimeout);
  }

  /// 查询系统已连接且暴露指定 service 的设备；iOS 冷恢复时优先于重新扫描。
  Future<List<BleDeviceInfo>> systemDevices({
    String serviceUuid = BoardGatt.serviceUuid,
  }) async {
    await start();
    final transport = _transport;
    if (transport is! BoardSystemDeviceTransport) return const [];
    return (transport as BoardSystemDeviceTransport).systemDevices(
      serviceUuid: serviceUuid,
    );
  }

  Future<void> connect(BleDeviceInfo device) async {
    await start();
    _target = device;
    _userDisconnect = false;
    _reconnectGeneration++;
    await _connectTarget(device, autoConnect: false);
  }

  Future<void> _connectTarget(
    BleDeviceInfo device, {
    required bool autoConnect,
  }) async {
    _events.add(const ReconnectEvent(ReconnectState.connecting));
    await _transport.connect(device, autoConnect: autoConnect);
    _markConnected();
    if (_config.notificationSettleDelay > Duration.zero) {
      await _clock.delay(_config.notificationSettleDelay);
    }
    if (_config.notifyAppOnlineOnConnect) {
      try {
        await notifyAppOnline(true);
      } on BoardException catch (error) {
        _events.add(BoardErrorEvent('连接后上报 App 在线失败：${error.message}'));
      }
    }
  }

  Future<void> disconnect() async {
    _userDisconnect = true;
    _reconnectGeneration++;
    await _transport.disconnect();
    _markDisconnected('用户主动断开');
  }

  Future<void> shutdown() async {
    if (_disposed) return;
    _userDisconnect = true;
    _reconnectGeneration++;
    if (_connected && _config.notifyAppOnlineOnConnect) {
      try {
        await notifyAppOnline(false);
      } on Object {
        // 下线是 best-effort，不能阻止资源释放。
      }
    }
    await _stateSubscription?.cancel();
    await _notificationSubscription?.cancel();
    _stateSubscription = null;
    _notificationSubscription = null;
    if (_connected) await _transport.disconnect();
    _markDisconnected('SDK shutdown');
    _disposed = true;
    await _transport.dispose();
    await _events.close();
    await _audioFrames.close();
  }

  Future<DeviceInfo> readDeviceInfo() =>
      _enqueueCommand(
        BoardProtocol.getDeviceInfo(),
        BoardCommand.getDeviceInfo,
        BoardProtocol.parseDeviceInfo,
        requiredPayload: BoardGatt.deviceInfoGattBytes,
      ).then((info) {
        _events.add(DeviceInfoEvent(info));
        return info;
      });

  Future<KeyConfig> readKeyConfig() => _enqueueCommand(
    BoardProtocol.getKeyConfig(),
    BoardCommand.getKeyConfig,
    BoardProtocol.parseKeyConfig,
    requiredPayload: 63,
  );

  Future<void> writeKeyConfig(KeyConfig config) => _enqueueCommand<void>(
    BoardProtocol.setKeyConfig(config.bytes),
    BoardCommand.setKeyConfig,
    (response) =>
        BoardProtocol.parseSuccess(response, BoardCommand.setKeyConfig),
    requiredPayload: BoardGatt.keyConfigGattBytes,
  );

  Future<WorkMode> getWorkMode() => _enqueueCommand(
    BoardProtocol.getWorkMode(),
    BoardCommand.status,
    BoardProtocol.parseWorkMode,
  );

  Future<bool> getSilentRecord() => _enqueueCommand(
    BoardProtocol.getSilentRecord(),
    BoardCommand.getSilentRecord,
    (response) => BoardProtocol.parseBooleanResponse(
      response,
      BoardCommand.getSilentRecord,
    ),
  );

  Future<bool> setSilentRecord(bool enabled) => _enqueueCommand(
    BoardProtocol.setSilentRecord(enabled),
    BoardCommand.setSilentRecord,
    (response) => BoardProtocol.parseBooleanResponse(
      response,
      BoardCommand.setSilentRecord,
    ),
  );

  Future<SleepTimeout> getSleepTimeout() => _enqueueCommand(
    BoardProtocol.getSleepTimeout(),
    BoardCommand.getSleepTimeout,
    (response) =>
        BoardProtocol.parseSleepTimeout(response, BoardCommand.getSleepTimeout),
  );

  Future<SleepTimeout> setSleepTimeout(SleepTimeout timeout) => _enqueueCommand(
    BoardProtocol.setSleepTimeout(timeout),
    BoardCommand.setSleepTimeout,
    (response) =>
        BoardProtocol.parseSleepTimeout(response, BoardCommand.setSleepTimeout),
  );

  Future<void> notifyAppOnline(bool online) => _enqueueCommand<void>(
    BoardProtocol.notifyAppOnline(online),
    BoardCommand.appOnlineNotify,
    (response) =>
        BoardProtocol.parseSuccess(response, BoardCommand.appOnlineNotify),
  );

  Future<bool> getAppOnline() => _enqueueCommand(
    BoardProtocol.getAppOnline(),
    BoardCommand.getAppOnline,
    (response) =>
        BoardProtocol.parseBooleanResponse(response, BoardCommand.getAppOnline),
  );

  Future<String> getOpenUrl() => _enqueueCommand(
    BoardProtocol.getOpenUrl(),
    BoardCommand.getOpenUrl,
    BoardProtocol.parseOpenUrl,
  );

  Future<void> setOpenUrl(String url) => _enqueueCommand<void>(
    BoardProtocol.setOpenUrl(url),
    BoardCommand.setOpenUrl,
    (response) => BoardProtocol.parseSuccess(response, BoardCommand.setOpenUrl),
    requiredPayload: 63,
  );

  Future<AudioCapabilities> queryAudioCapabilities() async {
    final epoch = _connectionEpoch;
    try {
      final capabilities = await _enqueueCommand(
        BoardProtocol.getAudioCapabilities(),
        BoardCommand.getAudioCapabilities,
        BoardProtocol.parseAudioCapabilities,
      );
      if (_connected && _connectionEpoch == epoch) {
        _audioCapabilityState = AudioCapabilityReady(capabilities);
      }
      return capabilities;
    } on BoardException {
      if (_connected && _connectionEpoch == epoch) {
        _audioCapabilityState = const AudioCapabilityUnavailable();
      }
      rethrow;
    }
  }

  Future<AudioStreamState> controlAudioStream({
    required AudioStreamAction action,
    required AudioStreamScope scope,
    required int leaseId,
    required int ttlMs,
  }) async {
    if (action != AudioStreamAction.stop) {
      await _requirePayload(BoardGatt.versionedAudioGattBytes);
      final capabilityState = _audioCapabilityState;
      if (capabilityState is! AudioCapabilityReady ||
          !capabilityState.capabilities.supportsBleGatt) {
        throw const BoardUnsupportedException('当前连接未确认支持版本化 BLE 板载音频');
      }
    }
    final state = await _enqueueCommand(
      BoardProtocol.audioStreamControl(
        action: action,
        scope: scope,
        leaseId: leaseId,
        ttlMs: ttlMs,
      ),
      BoardCommand.audioStreamControl,
      BoardProtocol.parseAudioStreamState,
    );
    if (!state.matchesRequest(action: action, scope: scope, leaseId: leaseId)) {
      _closeAudioRoute();
      throw const BoardProtocolException(
        '音频 lease ack 与请求 owner/scope/lease 不匹配',
      );
    }
    if (action == AudioStreamAction.stop) {
      _closeAudioRoute();
    } else {
      _openAudioRoute(legacy: false);
    }
    return state;
  }

  /// 旧固件逃生口：只开放 session 原始音频，不声明 capability 或 timeline 支持。
  void startLegacySessionAudio() {
    if (!_connected) throw const BoardDisconnectedException();
    _requirePayloadNow(59);
    _openAudioRoute(legacy: true);
  }

  void stopLegacySessionAudio() {
    if (_legacyAudio) _closeAudioRoute();
  }

  Future<T> _enqueueCommand<T>(
    Uint8List bytes,
    int expectedCommand,
    T Function(List<int>) parser, {
    int? requiredPayload,
  }) {
    final result = Completer<T>();
    final scheduled = _commandTail.then((_) async {
      try {
        if (requiredPayload != null) await _requirePayload(requiredPayload);
        final response = await _executeCommand(bytes, expectedCommand);
        result.complete(parser(response));
      } catch (error, stackTrace) {
        if (!result.isCompleted) result.completeError(error, stackTrace);
      }
    });
    _commandTail = scheduled.catchError((Object _) {});
    return result.future;
  }

  Future<Uint8List> _executeCommand(
    Uint8List bytes,
    int expectedCommand,
  ) async {
    if (!_connected) throw const BoardDisconnectedException();
    final completer = Completer<Uint8List>();
    _pending = _PendingResponse(expectedCommand, completer);
    try {
      await _transport.write(bytes);
      final timeout = _clock.delay(_config.commandTimeout).then<Uint8List>((_) {
        throw BoardTimeoutException(
          '命令 0x${expectedCommand.toRadixString(16)} 响应超时',
        );
      });
      return await Future.any([completer.future, timeout]);
    } finally {
      if (identical(_pending?.completer, completer)) _pending = null;
    }
  }

  Future<void> _requirePayload(int requiredBytes) async {
    if (!_connected) throw const BoardDisconnectedException();
    if (_transport.maxGattPayload >= requiredBytes) return;

    final result = Completer<void>();
    late final StreamSubscription<int> payloadSubscription;
    late final StreamSubscription<BoardTransportState> stateSubscription;
    payloadSubscription = _transport.maxGattPayloads.listen((payload) {
      if (payload >= requiredBytes && !result.isCompleted) result.complete();
    });
    stateSubscription = _transport.connectionStates.listen((state) {
      if (state == BoardTransportState.disconnected && !result.isCompleted) {
        result.completeError(const BoardDisconnectedException());
      }
    });
    unawaited(
      _clock.delay(_config.mtuNegotiationTimeout).then((_) {
        if (!result.isCompleted) {
          result.completeError(
            BoardMtuException(
              requiredBytes: requiredBytes,
              actualBytes: _transport.maxGattPayload,
            ),
          );
        }
      }),
    );
    try {
      await result.future;
    } finally {
      await payloadSubscription.cancel();
      await stateSubscription.cancel();
    }
  }

  void _requirePayloadNow(int requiredBytes) {
    final actual = _transport.maxGattPayload;
    if (actual < requiredBytes) {
      throw BoardMtuException(
        requiredBytes: requiredBytes,
        actualBytes: actual,
      );
    }
  }

  void _onNotification(BoardNotification notification) {
    switch (notification.type) {
      case BoardNotificationType.event:
        _onEventNotification(notification.data);
      case BoardNotificationType.audio:
        _onAudioNotification(notification.data);
    }
  }

  void _onEventNotification(Uint8List data) {
    if (data.length < 2) {
      _events.add(const BoardErrorEvent('GATT Event 过短'));
      return;
    }
    final command = data[0];
    final pending = _pending;
    if (pending != null && pending.command == command) {
      if (!pending.completer.isCompleted) pending.completer.complete(data);
      return;
    }

    try {
      if (command == BoardCommand.consumer && data.length >= 4) {
        final value = data[2] | (data[3] << 8);
        for (final event in _input.handleConsumer(value, _clock.now)) {
          _events.add(event);
        }
      } else if (command == BoardCommand.status &&
          data.length >= 4 &&
          data[2] == BoardCommand.workModeData) {
        final mode = WorkMode.fromValue(data[3]);
        if (mode != null) _events.add(ModeChangeEvent(mode));
      }
    } on BoardException catch (error) {
      _events.add(BoardErrorEvent(error.message));
    }
  }

  void _onAudioNotification(Uint8List data) {
    if (!_audioEnabled) return;
    try {
      var frame = BoardProtocol.parseAudioPacket(data, legacy: _legacyAudio);
      if (!_legacyAudio && frame.sequence == null) return;
      var gap = 0;
      final sequence = frame.sequence;
      if (sequence != null && _lastAudioSequence != null) {
        final delta = (sequence - _lastAudioSequence!) & 0xFFFF;
        if (delta == 0 || delta > 0x7FFF) return;
        if (delta > 1) gap = delta - 1;
      }
      if (sequence != null) _lastAudioSequence = sequence;
      frame = frame.copyWith(
        sequenceGapFrames: gap,
        connectionEpoch: _connectionEpoch,
        routeEpoch: _routeEpoch,
      );
      _audioFrames.add(frame);
    } on BoardException catch (error) {
      _events.add(BoardErrorEvent('音频包解析失败：${error.message}'));
    }
  }

  void _onTransportState(BoardTransportState state) {
    switch (state) {
      case BoardTransportState.connecting:
        _events.add(const ReconnectEvent(ReconnectState.connecting));
      case BoardTransportState.connected:
        _markConnected();
      case BoardTransportState.disconnected:
        final wasConnected = _connected;
        _markDisconnected('BLE 链路断开');
        if (wasConnected && !_userDisconnect && _config.autoReconnect) {
          _scheduleReconnect();
        }
    }
  }

  void _markConnected() {
    if (_connected) return;
    _connected = true;
    _connectionEpoch++;
    _audioCapabilityState = const AudioCapabilityUnqueried();
    _events.add(const ConnectionEvent(connected: true));
    _events.add(const ReconnectEvent(ReconnectState.connected));
    if (_transport.maxGattPayload < BoardGatt.keyConfigGattBytes) {
      _events.add(
        BoardCapabilityWarningEvent(
          '当前 GATT 有效载荷 ${_transport.maxGattPayload} 字节，设备信息/按键配置/音频部分能力受限',
        ),
      );
    }
  }

  void _markDisconnected(String reason) {
    if (!_connected && _pending == null) return;
    _connected = false;
    _audioCapabilityState = const AudioCapabilityUnqueried();
    _closeAudioRoute();
    final pending = _pending;
    if (pending != null && !pending.completer.isCompleted) {
      pending.completer.completeError(BoardDisconnectedException(reason));
    }
    _pending = null;
    for (final event in _input.releaseAll()) {
      if (!_events.isClosed) _events.add(event);
    }
    if (!_events.isClosed) {
      _events.add(ConnectionEvent(connected: false, reason: reason));
    }
  }

  void _scheduleReconnect() {
    final target = _target;
    if (target == null) return;
    final generation = ++_reconnectGeneration;
    unawaited(() async {
      for (var attempt = 1; attempt <= 5; attempt++) {
        if (_disposed ||
            _userDisconnect ||
            generation != _reconnectGeneration) {
          return;
        }
        _events.add(
          ReconnectEvent(ReconnectState.waitingForDevice, attempt: attempt),
        );
        final seconds = 1 << (attempt - 1).clamp(0, 4);
        await _clock.delay(Duration(seconds: seconds));
        try {
          await _connectTarget(target, autoConnect: true);
          return;
        } on Object catch (error) {
          _events.add(
            ReconnectEvent(
              ReconnectState.connecting,
              attempt: attempt,
              message: '$error',
            ),
          );
        }
      }
      _events.add(const ReconnectEvent(ReconnectState.suppressed));
    }());
  }

  void _openAudioRoute({required bool legacy}) {
    _routeEpoch++;
    _audioEnabled = true;
    _legacyAudio = legacy;
    _lastAudioSequence = null;
  }

  void _closeAudioRoute() {
    if (_audioEnabled) _routeEpoch++;
    _audioEnabled = false;
    _legacyAudio = false;
    _lastAudioSequence = null;
  }
}
