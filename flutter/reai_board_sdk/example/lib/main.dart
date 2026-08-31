import 'dart:async';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:reai_board_sdk/reai_board_sdk.dart';

import 'dashboard_widgets.dart';

export 'dashboard_widgets.dart' show HardwareInputPanel;

const exampleBuildLabel = '1.0.0 (3)';

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
    debugShowCheckedModeBanner: false,
    theme: ThemeData(
      useMaterial3: true,
      colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF5968E8)),
      scaffoldBackgroundColor: const Color(0xFFF6F7FB),
      cardTheme: const CardThemeData(
        margin: EdgeInsets.zero,
        elevation: 0,
        color: Colors.white,
      ),
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
  final Set<int> _pressedKeys = {};
  final Set<int> _dirtyKeys = {};
  final Map<int, int> _eventCounts = {};
  List<BleDeviceInfo> _devices = const [];
  StreamSubscription<BoardEvent>? _eventSubscription;
  StreamSubscription<EncodedAudioFrame>? _audioSubscription;
  StreamSubscription<List<BleDeviceInfo>>? _scanSubscription;
  StreamSubscription<int>? _mtuSubscription;
  Timer? _heartbeat;
  BleDeviceInfo? _connectedDevice;
  DeviceInfo? _deviceInfo;
  KeyConfig? _keyConfig;
  WorkMode? _selectedMode;
  int _selectedKeyIndex = 0;
  String _lastEvent = '连接设备后，按实体键开始验收';
  bool _busy = false;
  bool _scanning = false;
  bool _audioRunning = false;
  int _maxGattPayload = 20;
  int _audioFrameCount = 0;
  int _audioGapCount = 0;

  @override
  void initState() {
    super.initState();
    _log('页面启动，测试包=$exampleBuildLabel，12 键/配置/音频验收台');
    unawaited(_board.start());
    _eventSubscription = _board.events.listen(
      _handleBoardEvent,
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
          if (mounted) setState(() {});
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
    _scanSubscription = _board.scanResults.listen(
      (devices) {
        if (!mounted) return;
        final knownIds = _devices.map((device) => device.id).toSet();
        setState(() => _devices = devices);
        for (final device in devices.where(
          (device) => !knownIds.contains(device.id),
        )) {
          _log(
            '扫描发现 name=${device.name} id=${device.id} '
            'rssi=${device.rssi}dBm，立即显示',
          );
        }
      },
      onError: (Object error, StackTrace stackTrace) {
        _logError('扫描结果流异常', error, stackTrace);
      },
    );
    _mtuSubscription = _board.maxGattPayloads.listen(
      (payload) {
        if (mounted) setState(() => _maxGattPayload = payload);
        _log('GATT 有效载荷已更新为 $payload 字节');
      },
      onError: (Object error, StackTrace stackTrace) {
        _logError('MTU 变化流异常', error, stackTrace);
      },
    );
  }

  void _handleBoardEvent(BoardEvent event) {
    if (mounted) {
      setState(() {
        switch (event) {
          case ConnectionEvent(:final connected):
            if (!connected) {
              _connectedDevice = null;
              _deviceInfo = null;
              _keyConfig = null;
              _pressedKeys.clear();
              _dirtyKeys.clear();
              _audioRunning = false;
            }
          case KeyPressEvent(:final keyIndex, :final pressed):
            if (pressed) {
              _pressedKeys.add(keyIndex);
              if (keyIndex >= 0 && keyIndex < KeyConfig.activeKeyCount) {
                _selectedKeyIndex = keyIndex;
              }
              _eventCounts.update(
                keyIndex,
                (value) => value + 1,
                ifAbsent: () => 1,
              );
            } else {
              _pressedKeys.remove(keyIndex);
            }
            final key =
                keyIndex >= 0 && keyIndex < BoardPhysicalKey.values.length
                ? BoardPhysicalKey.values[keyIndex].name
                : 'KEY$keyIndex';
            _lastEvent = '$key ${pressed ? '按下' : '释放'}';
          case ModeChangeEvent(:final mode):
            _selectedMode = mode;
            _lastEvent = '模式切换为 ${mode.label}';
          case DeviceInfoEvent(:final info):
            _deviceInfo = info;
          default:
            break;
        }
      });
    }
    _log(formatBoardEventForLog(event));
  }

  Future<void> _scan() async {
    HapticFeedback.lightImpact();
    setState(() => _scanning = true);
    try {
      await _run(() async {
        _log('扫描开始，timeout=10s，目标前缀=REAI_VB_');
        final devices = await _board.scan();
        if (mounted) setState(() => _devices = devices);
        _log('扫描完成，发现 ${devices.length} 台设备');
      });
    } finally {
      if (mounted) setState(() => _scanning = false);
    }
  }

  Future<void> _connect(BleDeviceInfo device) async {
    HapticFeedback.mediumImpact();
    await _run(() async {
      _log('连接开始 name=${device.name} id=${device.id} rssi=${device.rssi}');
      await _board.connect(device);
      if (mounted) {
        setState(() {
          _connectedDevice = device;
          _maxGattPayload = _board.maxGattPayload;
        });
      }
      _log('GATT ready，payload=${_board.maxGattPayload}B，开始读取设备状态');
      final info = await _board.readDeviceInfo();
      final config = await _readKeyConfig();
      final mode = await _board.getWorkMode();
      if (mounted) {
        setState(() {
          _deviceInfo = info;
          _keyConfig = config;
          _selectedMode = mode;
          _dirtyKeys.clear();
        });
      }
      _log(
        '已连接 ${device.name}，chip=${info.chipId}，'
        'firmware=${info.firmwareVersion}，battery=${info.batteryLevel}%，'
        '按键配置=${config.activeBindings.length}项，mode=${mode.label}',
      );
    });
  }

  Future<KeyConfig> _readKeyConfig() async {
    _log('读取按键配置，期望响应 63B，当前 GATT payload=${_board.maxGattPayload}B');
    final config = await _board.readKeyConfig();
    _log(
      '按键配置读取成功：${config.activeBindings.map((binding) => binding.description).join(' / ')}',
    );
    return config;
  }

  Future<void> _refreshKeyConfig() async {
    HapticFeedback.lightImpact();
    if (_dirtyKeys.isNotEmpty &&
        !await _confirm(
          title: '放弃本地修改？',
          message: '重新读取会丢弃 ${_dirtyKeys.length} 项尚未写入的修改。',
          confirmLabel: '放弃并读取',
        )) {
      return;
    }
    await _run(() async {
      final config = await _readKeyConfig();
      if (mounted) {
        setState(() {
          _keyConfig = config;
          _dirtyKeys.clear();
        });
      }
    });
  }

  void _changeBinding(int index, KeyBinding binding) {
    final config = _keyConfig;
    if (config == null) return;
    HapticFeedback.selectionClick();
    setState(() {
      _keyConfig = config.copyWithActiveBinding(index, binding);
      _selectedKeyIndex = index;
      _dirtyKeys.add(index);
    });
    _log('本地修改 KEY$index → ${binding.description}，尚未写入设备');
  }

  Future<void> _restoreDefaults() async {
    if (!await _confirm(
      title: '恢复出厂映射？',
      message: '先在本地生成 12 项默认映射；仍需再次点击“写入设备”并确认才会落盘。',
      confirmLabel: '生成默认映射',
    )) {
      return;
    }
    HapticFeedback.mediumImpact();
    setState(() {
      _keyConfig = KeyConfig.factoryDefaults();
      _dirtyKeys
        ..clear()
        ..addAll(
          List<int>.generate(KeyConfig.activeKeyCount, (index) => index),
        );
    });
    _log('已在本地恢复 12 项出厂映射，等待用户确认写入');
  }

  Future<void> _writeKeyConfig() async {
    final draft = _keyConfig;
    final dirty = Set<int>.from(_dirtyKeys);
    if (draft == null || dirty.isEmpty) return;
    if (!await _confirm(
      title: '写入按键配置？',
      message: '这是持久化操作。将写入 ${dirty.length} 个按键，并在写入后重新读取核验。',
      confirmLabel: '确认写入',
    )) {
      return;
    }
    HapticFeedback.mediumImpact();
    await _run(() async {
      _log('写入前重新读取最新配置，只合并 ${dirty.length} 个本地修改槽位');
      var merged = await _readKeyConfig();
      for (final index in dirty) {
        merged = merged.copyWithActiveBinding(
          index,
          draft.activeBindings[index],
        );
      }
      await _board.writeKeyConfig(merged);
      _log('SET 0x16 返回成功，开始 GET 0x15 回读核验');
      final verified = await _readKeyConfig();
      for (final index in dirty) {
        if (verified.activeBindings[index] != draft.activeBindings[index]) {
          throw BoardProtocolException('KEY$index 写入后回读不一致');
        }
      }
      if (mounted) {
        setState(() {
          _keyConfig = verified;
          _dirtyKeys.clear();
        });
      }
      _log('按键配置写入并回读验证成功，共 ${dirty.length} 项');
    });
  }

  Future<bool> _confirm({
    required String title,
    required String message,
    required String confirmLabel,
  }) async {
    if (!mounted) return false;
    return await showDialog<bool>(
          context: context,
          builder: (context) => AlertDialog(
            title: Text(title),
            content: Text(message),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(context, false),
                child: const Text('取消'),
              ),
              FilledButton(
                onPressed: () => Navigator.pop(context, true),
                child: Text(confirmLabel),
              ),
            ],
          ),
        ) ??
        false;
  }

  Future<void> _disconnect() async {
    HapticFeedback.mediumImpact();
    await _run(() async {
      await _board.disconnect();
      _log('已主动断开设备');
    });
  }

  Future<void> _startAudio() async {
    HapticFeedback.mediumImpact();
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
      await _board.controlAudioStream(
        action: AudioStreamAction.start,
        scope: AudioStreamScope.session,
        leaseId: _leaseId,
        ttlMs: ttl,
      );
      _heartbeat?.cancel();
      _heartbeat = Timer.periodic(Duration(milliseconds: ttl ~/ 2), (_) {
        unawaited(_sendHeartbeat(ttl));
      });
      _audioFrameCount = 0;
      _audioGapCount = 0;
      if (mounted) setState(() => _audioRunning = true);
      _log('音频 lease 启动成功 id=0x${_leaseId.toRadixString(16)} ttl=${ttl}ms');
    });
  }

  Future<void> _sendHeartbeat(int ttl) async {
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
  }

  Future<void> _stopAudio() async {
    HapticFeedback.mediumImpact();
    _heartbeat?.cancel();
    _heartbeat = null;
    await _run(() async {
      await _board.controlAudioStream(
        action: AudioStreamAction.stop,
        scope: AudioStreamScope.session,
        leaseId: _leaseId,
        ttlMs: 0,
      );
      if (mounted) setState(() => _audioRunning = false);
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
      if (_logs.length > 200) _logs.removeLast();
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
    unawaited(_scanSubscription?.cancel());
    unawaited(_mtuSubscription?.cancel());
    unawaited(_board.shutdown());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('ReAI Board SDK $exampleBuildLabel')),
    body: AcceptanceDashboard(
      connected: _board.isConnected,
      device: _connectedDevice,
      info: _deviceInfo,
      maxGattPayload: _maxGattPayload,
      lastEvent: _lastEvent,
      busy: _busy,
      scanning: _scanning,
      devices: _devices,
      keyConfig: _keyConfig,
      pressedKeys: _pressedKeys,
      eventCounts: _eventCounts,
      selectedMode: _selectedMode,
      selectedKeyIndex: _selectedKeyIndex,
      dirtyKeys: _dirtyKeys,
      audioRunning: _audioRunning,
      audioFrameCount: _audioFrameCount,
      audioGapCount: _audioGapCount,
      logs: _logs,
      onScan: _scan,
      onConnect: _connect,
      onDisconnect: _disconnect,
      onRefreshKeyConfig: _refreshKeyConfig,
      onSelectKey: (index) {
        HapticFeedback.selectionClick();
        setState(() => _selectedKeyIndex = index);
      },
      onBindingChanged: _changeBinding,
      onWriteKeyConfig: _writeKeyConfig,
      onRestoreDefaults: _restoreDefaults,
      onToggleAudio: _audioRunning ? _stopAudio : _startAudio,
      onClearLogs: () {
        HapticFeedback.lightImpact();
        setState(_logs.clear);
      },
    ),
  );
}
