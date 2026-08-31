import 'dart:async';

import 'package:flutter/material.dart';
import 'package:reai_board_sdk/reai_board_sdk.dart';

void main() => runApp(const BoardSdkExampleApp());

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

  @override
  void initState() {
    super.initState();
    unawaited(_board.start());
    _eventSubscription = _board.events.listen((event) => _log('$event'));
    _audioSubscription = _board.audioFrames.listen(
      (frame) => _log(
        'mSBC ${frame.payload.length}B seq=${frame.sequence} gap=${frame.sequenceGapFrames}',
      ),
    );
  }

  Future<void> _scan() async {
    await _run(() async {
      final devices = await _board.scan();
      setState(() => _devices = devices);
      _log('发现 ${devices.length} 台设备');
    });
  }

  Future<void> _connect(BleDeviceInfo device) async {
    await _run(() async {
      await _board.connect(device);
      final info = await _board.readDeviceInfo();
      _log(
        '已连接 ${device.name}，chip=${info.chipId}，'
        'firmware=${info.firmwareVersion}，battery=${info.batteryLevel}%',
      );
    });
  }

  Future<void> _startAudio() async {
    await _run(() async {
      final capabilities = await _board.queryAudioCapabilities();
      if (!capabilities.supportsBleGatt) {
        throw const BoardUnsupportedException('固件没有版本化 BLE mSBC 能力');
      }
      final ttl = capabilities.defaultTtlMs;
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
          } on Object catch (error) {
            _heartbeat?.cancel();
            _heartbeat = null;
            if (mounted) setState(() => _audioRunning = false);
            _log('音频 lease 心跳失败：$error');
          }
        }());
      });
      setState(() => _audioRunning = true);
    });
  }

  Future<void> _stopAudio() async {
    _heartbeat?.cancel();
    _heartbeat = null;
    await _run(() async {
      await _board.controlAudioStream(
        action: AudioStreamAction.stop,
        scope: AudioStreamScope.session,
        leaseId: _leaseId,
        ttlMs: 0,
      );
      setState(() => _audioRunning = false);
    });
  }

  Future<void> _run(Future<void> Function() action) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await action();
    } on Object catch (error) {
      _log('错误：$error');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  void _log(String message) {
    if (!mounted) return;
    setState(() {
      _logs.insert(0, message);
      if (_logs.length > 30) _logs.removeLast();
    });
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
