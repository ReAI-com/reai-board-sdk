import 'dart:async';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:reai_board_sdk/reai_board_sdk.dart';

void main() {
  FlutterError.onError = (details) {
    FlutterError.presentError(details);
    debugPrint(
      '[ReAIBoardSDK][FlutterError] ${details.exceptionAsString()}\n'
      '${details.stack ?? StackTrace.current}',
    );
  };
  PlatformDispatcher.instance.onError = (error, stackTrace) {
    debugPrint('[ReAIBoardSDK][PlatformError] $error\n$stackTrace');
    return true;
  };
  runApp(const BoardSdkExampleApp());
}

String formatBoardEventForLog(BoardEvent event) => switch (event) {
  ConnectionEvent(:final connected, :final reason) =>
    'connection connected=$connected reason=${reason ?? '-'}',
  ReconnectEvent(:final state, :final attempt, :final message) =>
    'reconnect state=${state.name} attempt=${attempt ?? '-'} '
        'message=${message ?? '-'}',
  KeyPressEvent(
    :final keyIndex,
    :final keyName,
    :final keyValue,
    :final pressed,
    :final source,
  ) =>
    'key index=$keyIndex name=$keyName value=0x${keyValue.toRadixString(16)} '
        'pressed=$pressed source=${source.name}',
  ComboKeyEvent(:final keys) => 'combo keys=$keys',
  AiVoiceKeyEvent(:final pressed) => 'ai_voice pressed=$pressed',
  ModeChangeEvent(:final mode, :final source) =>
    'mode value=${mode.label} source=${source.name}',
  DeviceInfoEvent(:final info) =>
    'device_info chip=${info.chipId} firmware=${info.firmwareVersion} '
        'battery=${info.batteryLevel} charging=${info.batteryCharging}',
  BoardErrorEvent(:final message, :final recoverable) =>
    'sdk_error recoverable=$recoverable message=$message',
  BoardCapabilityWarningEvent(:final message) =>
    'capability_warning message=$message',
};

class BoardSdkExampleApp extends StatelessWidget {
  const BoardSdkExampleApp({super.key});

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'ReAI Board SDK',
    theme: ThemeData(
      colorScheme: ColorScheme.fromSeed(seedColor: Colors.indigo),
    ),
    home: const BoardSdkPage(),
  );
}

class BoardSdkPage extends StatefulWidget {
  const BoardSdkPage({super.key});

  @override
  State<BoardSdkPage> createState() => _BoardSdkPageState();
}

class _BoardSdkPageState extends State<BoardSdkPage> {
  static const _leaseId = 0x4D4F424C;
  final BoardDevice _board = BoardDevice(
    transport: FlutterBluePlusBoardTransport(),
  );
  final List<String> _logs = [];
  List<BleDeviceInfo> _devices = const [];
  StreamSubscription<BoardEvent>? _eventSubscription;
  StreamSubscription<EncodedAudioFrame>? _audioSubscription;
  Timer? _heartbeat;
  bool _busy = false;
  bool _audioRunning = false;
  int _audioFrameCount = 0;
  int _audioGapCount = 0;

  @override
  void initState() {
    super.initState();
    _log('页面启动，开始监听 SDK 事件');
    unawaited(_board.start());
    _eventSubscription = _board.events.listen(
      (event) => _log(formatBoardEventForLog(event)),
      onError: (Object error, StackTrace stackTrace) {
        _logError('事件流异常', error, stackTrace);
      },
    );
    _audioSubscription = _board.audioFrames.listen(
      (frame) {
        _audioFrameCount++;
        _audioGapCount += frame.sequenceGapFrames;
        if (_audioFrameCount == 1 ||
            _audioFrameCount % 50 == 0 ||
            frame.discontinuity) {
          _log(
            'mSBC frames=$_audioFrameCount payload=${frame.payload.length}B '
            'seq=${frame.sequence} total_gap=$_audioGapCount '
            'device_discontinuity=${frame.deviceDiscontinuity}',
          );
        }
      },
      onError: (Object error, StackTrace stackTrace) {
        _logError('音频流异常', error, stackTrace);
      },
    );
  }

  Future<void> _scan() async {
    await _run(() async {
      _log('扫描开始，timeout=10s，目标前缀=REAI_VB_');
      final devices = await _board.scan();
      setState(() => _devices = devices);
      _log(
        '扫描完成，发现 ${devices.length} 台设备：'
        '${devices.map((device) => '${device.name}/${device.id}/${device.rssi}dBm').join(', ')}',
      );
    });
  }

  Future<void> _connect(BleDeviceInfo device) async {
    await _run(() async {
      _log('连接开始 name=${device.name} id=${device.id} rssi=${device.rssi}');
      await _board.connect(device);
      _log('GATT ready，开始读取设备信息');
      final info = await _board.readDeviceInfo();
      _log(
        '已连接 ${device.name}，chip=${info.chipId}，'
        'firmware=${info.firmwareVersion}，battery=${info.batteryLevel}%',
      );
    });
  }

  Future<void> _startAudio() async {
    await _run(() async {
      _log('查询音频 capability');
      final capabilities = await _board.queryAudioCapabilities();
      _log(
        '音频 capability protocol=${capabilities.protocolVersion} '
        'ble=${capabilities.bleGattMsbcV1} '
        'stream_control=${capabilities.streamControlV1} '
        'sequence=${capabilities.packetSequenceV1} '
        'ble_max_payload=${capabilities.bleMaxPayload} '
        'ttl=${capabilities.defaultTtlMs}/${capabilities.maxTtlMs}ms',
      );
      if (!capabilities.supportsBleGatt) {
        throw const BoardUnsupportedException('固件没有版本化 BLE mSBC 能力');
      }
      final ttl = capabilities.defaultTtlMs;
      _log('启动音频 lease id=0x${_leaseId.toRadixString(16)} ttl=${ttl}ms');
      await _board.controlAudioStream(
        action: AudioStreamAction.start,
        scope: AudioStreamScope.session,
        leaseId: _leaseId,
        ttlMs: ttl,
      );
      _heartbeat?.cancel();
      _heartbeat = Timer.periodic(Duration(milliseconds: ttl ~/ 2), (_) {
        unawaited(() async {
          try {
            await _board.controlAudioStream(
              action: AudioStreamAction.heartbeat,
              scope: AudioStreamScope.session,
              leaseId: _leaseId,
              ttlMs: ttl,
            );
            _log('音频 lease heartbeat 成功');
          } on Object catch (error, stackTrace) {
            _heartbeat?.cancel();
            _heartbeat = null;
            if (mounted) setState(() => _audioRunning = false);
            _logError('音频 lease 心跳失败', error, stackTrace);
          }
        }());
      });
      _audioFrameCount = 0;
      _audioGapCount = 0;
      setState(() => _audioRunning = true);
      _log('音频 lease 启动成功');
    });
  }

  Future<void> _stopAudio() async {
    _heartbeat?.cancel();
    _heartbeat = null;
    await _run(() async {
      _log('停止音频 lease');
      await _board.controlAudioStream(
        action: AudioStreamAction.stop,
        scope: AudioStreamScope.session,
        leaseId: _leaseId,
        ttlMs: 0,
      );
      setState(() => _audioRunning = false);
      _log('音频 lease 已停止');
    });
  }

  Future<void> _run(Future<void> Function() action) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await action();
    } on Object catch (error, stackTrace) {
      _logError('操作失败', error, stackTrace);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  void _log(String message) {
    final line = '${DateTime.now().toIso8601String()} $message';
    debugPrint('[ReAIBoardSDK] $line');
    if (!mounted) return;
    setState(() {
      _logs.insert(0, line);
      if (_logs.length > 100) _logs.removeLast();
    });
  }

  void _logError(String context, Object error, StackTrace stackTrace) {
    _log('$context：$error');
    debugPrint('[ReAIBoardSDK][StackTrace][$context]\n$stackTrace');
  }

  @override
  void dispose() {
    _heartbeat?.cancel();
    unawaited(_eventSubscription?.cancel());
    unawaited(_audioSubscription?.cancel());
    unawaited(_board.shutdown());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('ReAI-Vibe-Board SDK')),
    body: ListView(
      padding: const EdgeInsets.all(16),
      children: [
        FilledButton(
          onPressed: _busy ? null : _scan,
          child: Text(_busy ? '处理中…' : '扫描 REAI_VB_ 设备'),
        ),
        for (final device in _devices)
          ListTile(
            title: Text(device.name),
            subtitle: Text('${device.id}  RSSI ${device.rssi}'),
            trailing: const Icon(Icons.bluetooth),
            onTap: _busy ? null : () => _connect(device),
          ),
        const Divider(),
        FilledButton.tonal(
          onPressed: !_board.isConnected || _busy
              ? null
              : (_audioRunning ? _stopAudio : _startAudio),
          child: Text(_audioRunning ? '停止音频 lease' : '启动音频 lease'),
        ),
        const SizedBox(height: 16),
        const Text('事件日志', style: TextStyle(fontWeight: FontWeight.bold)),
        for (final log in _logs) Text(log),
      ],
    ),
  );
}
