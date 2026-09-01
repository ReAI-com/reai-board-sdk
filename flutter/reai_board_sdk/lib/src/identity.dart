import 'constants.dart';

/// 将设备身份响应中的完整 6 字节 MAC 规范化为外脑库存/绑定硬件 ID。
String normalizeVibeBoardHardwareId(String macAddress) {
  final compact = macAddress
      .replaceAll(RegExp(r'[^0-9A-Fa-f]'), '')
      .toUpperCase();
  if (compact.length != 12 || !RegExp(r'^[0-9A-F]{12}$').hasMatch(compact)) {
    throw ArgumentError.value(macAddress, 'macAddress', '必须是完整 6 字节 MAC');
  }
  return '${BoardGatt.devicePrefix}$compact';
}
