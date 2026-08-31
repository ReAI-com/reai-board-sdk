import 'dart:async';
import 'package:flutter/foundation.dart';
import 'package:flutter_blue_plus/flutter_blue_plus.dart';

import 'constants.dart';
import 'errors.dart';
import 'models.dart';
import 'transport.dart';

/// `flutter_blue_plus` 的薄 I/O 适配器；不包含协议和业务状态机。
final class FlutterBluePlusBoardTransport implements BoardBleTransport {
  final _states = StreamController<BoardTransportState>.broadcast();
  final _notifications = StreamController<BoardNotification>.broadcast();
  final _scanResults = StreamController<List<BleDeviceInfo>>.broadcast();
  final _maxGattPayloads = StreamController<int>.broadcast();

  StreamSubscription<List<ScanResult>>? _scanSubscription;
  StreamSubscription<BluetoothConnectionState>? _connectionSubscription;
  StreamSubscription<int>? _mtuSubscription;
  StreamSubscription<List<int>>? _eventSubscription;
  StreamSubscription<List<int>>? _audioSubscription;
  BluetoothDevice? _device;
  BluetoothCharacteristic? _commandCharacteristic;
  int _maxGattPayload = 20;
  bool _disposed = false;

  @override
  Stream<BoardTransportState> get connectionStates => _states.stream;

  @override
  Stream<BoardNotification> get notificationStream => _notifications.stream;

  @override
  Stream<List<BleDeviceInfo>> get scanResults => _scanResults.stream;

  @override
  int get maxGattPayload => _maxGattPayload;

  @override
  Stream<int> get maxGattPayloads => _maxGattPayloads.stream;

  @override
  Future<List<BleDeviceInfo>> scan({Duration? timeout}) async {
    _checkNotDisposed();
    final supported = await FlutterBluePlus.isSupported;
    if (!supported) throw const BoardTransportException('当前设备不支持 BLE');

    final found = <String, BleDeviceInfo>{};
    if (!_scanResults.isClosed) _scanResults.add(const []);
    await _scanSubscription?.cancel();
    _scanSubscription = FlutterBluePlus.onScanResults.listen((results) {
      var changed = false;
      for (final result in results) {
        final name = result.advertisementData.advName;
        if (!name.startsWith(BoardGatt.devicePrefix)) continue;
        final next = BleDeviceInfo(
          id: result.device.remoteId.str,
          name: name,
          rssi: result.rssi,
        );
        if (found[next.id] != next) {
          found[next.id] = next;
          changed = true;
        }
      }
      if (changed && !_scanResults.isClosed) {
        _scanResults.add(_sortedDevices(found));
      }
    });

    try {
      await FlutterBluePlus.startScan(
        timeout: timeout ?? const Duration(seconds: 10),
      );
      await FlutterBluePlus.isScanning.where((scanning) => !scanning).first;
    } on Object catch (error) {
      throw BoardTransportException('BLE 扫描失败', cause: error);
    } finally {
      await FlutterBluePlus.stopScan();
      await _scanSubscription?.cancel();
      _scanSubscription = null;
    }
    return _sortedDevices(found);
  }

  @override
  Future<void> connect(BleDeviceInfo device, {bool autoConnect = false}) async {
    _checkNotDisposed();
    await _cancelDeviceSubscriptions();
    _setMaxGattPayload(20);
    final bluetoothDevice = BluetoothDevice.fromId(device.id);
    _device = bluetoothDevice;
    _states.add(BoardTransportState.connecting);

    _connectionSubscription = bluetoothDevice.connectionState.listen((state) {
      if (_states.isClosed) return;
      switch (state) {
        case BluetoothConnectionState.connected:
          // GATT 链路已连不等于 SDK ready；服务发现和通知订阅完成后再发 connected。
          return;
        case BluetoothConnectionState.disconnected:
          _states.add(BoardTransportState.disconnected);
        default:
          _states.add(BoardTransportState.connecting);
      }
    });

    try {
      if (!bluetoothDevice.isConnected) {
        final connected = bluetoothDevice.connectionState
            .firstWhere((state) => state == BluetoothConnectionState.connected)
            .timeout(const Duration(seconds: 35));
        await bluetoothDevice.connect(
          autoConnect: autoConnect,
          mtu: null,
          timeout: const Duration(seconds: 35),
        );
        await connected;
      }

      _mtuSubscription = bluetoothDevice.mtu.listen(_updateMtu);
      _updateMtu(bluetoothDevice.mtuNow);

      if (defaultTargetPlatform == TargetPlatform.android) {
        try {
          await bluetoothDevice.requestMtu(BoardGatt.requestedMtu);
        } on Object {
          // MTU 是会话能力，不阻断短事件和短命令。
        }
      }
      _updateMtu(bluetoothDevice.mtuNow);

      final services = await bluetoothDevice.discoverServices();
      final service = services.where(
        (candidate) =>
            candidate.uuid.str.toLowerCase() == BoardGatt.serviceUuid,
      );
      if (service.isEmpty) {
        throw const BoardTransportException('未找到固件 Vendor GATT Service FE60');
      }
      BluetoothCharacteristic? command;
      BluetoothCharacteristic? event;
      BluetoothCharacteristic? audio;
      for (final characteristic in service.single.characteristics) {
        switch (characteristic.uuid.str.toLowerCase()) {
          case BoardGatt.commandUuid:
            command = characteristic;
          case BoardGatt.eventUuid:
            event = characteristic;
          case BoardGatt.audioUuid:
            audio = characteristic;
        }
      }
      if (command == null || event == null || audio == null) {
        throw const BoardTransportException('FE61/FE62/FE63 特征不完整');
      }
      _commandCharacteristic = command;

      // 必须先监听再开通知，避免丢掉第一帧；不用 lastValueStream，避免重放旧值。
      _eventSubscription = event.onValueReceived.listen((value) {
        if (!_notifications.isClosed) {
          _notifications.add(
            BoardNotification.event(Uint8List.fromList(value)),
          );
        }
      });
      _audioSubscription = audio.onValueReceived.listen((value) {
        if (!_notifications.isClosed) {
          _notifications.add(
            BoardNotification.audio(Uint8List.fromList(value)),
          );
        }
      });
      await event.setNotifyValue(true);
      await audio.setNotifyValue(true);
      if (!_states.isClosed) _states.add(BoardTransportState.connected);
    } on BoardException {
      await disconnect();
      rethrow;
    } on Object catch (error) {
      await disconnect();
      throw BoardTransportException('连接 ReAI-Vibe-Board 失败', cause: error);
    }
  }

  @override
  Future<void> write(Uint8List data) async {
    final characteristic = _commandCharacteristic;
    if (characteristic == null || _device?.isConnected != true) {
      throw const BoardDisconnectedException();
    }
    if (data.length > _maxGattPayload) {
      throw BoardMtuException(
        requiredBytes: data.length,
        actualBytes: _maxGattPayload,
      );
    }
    try {
      await characteristic.write(data, withoutResponse: false);
    } on Object catch (error) {
      throw BoardTransportException('FE61 命令写入失败', cause: error);
    }
  }

  @override
  Future<void> disconnect() async {
    final device = _device;
    _commandCharacteristic = null;
    await _eventSubscription?.cancel();
    await _audioSubscription?.cancel();
    await _mtuSubscription?.cancel();
    _eventSubscription = null;
    _audioSubscription = null;
    _mtuSubscription = null;
    if (device != null) {
      try {
        await device.disconnect();
      } on Object {
        // 幂等释放。
      }
    }
    await _connectionSubscription?.cancel();
    _connectionSubscription = null;
    _device = null;
    _setMaxGattPayload(20);
  }

  Future<void> _cancelDeviceSubscriptions() async {
    await _connectionSubscription?.cancel();
    await _mtuSubscription?.cancel();
    await _eventSubscription?.cancel();
    await _audioSubscription?.cancel();
    _connectionSubscription = null;
    _mtuSubscription = null;
    _eventSubscription = null;
    _audioSubscription = null;
    _commandCharacteristic = null;
  }

  @override
  Future<void> dispose() async {
    if (_disposed) return;
    _disposed = true;
    await _scanSubscription?.cancel();
    await disconnect();
    await _connectionSubscription?.cancel();
    await _states.close();
    await _notifications.close();
    await _scanResults.close();
    await _maxGattPayloads.close();
  }

  List<BleDeviceInfo> _sortedDevices(Map<String, BleDeviceInfo> found) =>
      found.values.toList()..sort((a, b) => b.rssi.compareTo(a.rssi));

  void _updateMtu(int mtu) {
    _setMaxGattPayload((mtu - BoardGatt.attOverhead).clamp(0, 509));
  }

  void _setMaxGattPayload(int payload) {
    if (_maxGattPayload == payload) return;
    _maxGattPayload = payload;
    if (!_maxGattPayloads.isClosed) _maxGattPayloads.add(payload);
  }

  void _checkNotDisposed() {
    if (_disposed) throw const BoardTransportException('BLE transport 已释放');
  }
}
