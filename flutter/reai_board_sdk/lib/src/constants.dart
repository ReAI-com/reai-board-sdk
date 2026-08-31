/// ReAI-Vibe-Board Vendor GATT 常量。
abstract final class BoardGatt {
  static const devicePrefix = 'REAI_VB_';

  // 固件使用全零基 128 位 UUID，不是 Bluetooth Base UUID 的 16-bit 展开。
  static const serviceUuid = '00000000-0000-0000-0000-00000000fe60';
  static const commandUuid = '00000000-0000-0000-0000-00000000fe61';
  static const eventUuid = '00000000-0000-0000-0000-00000000fe62';
  static const audioUuid = '00000000-0000-0000-0000-00000000fe63';

  static const requestedMtu = 247;
  static const attOverhead = 3;
  static const keyConfigGattBytes = 62;
  static const deviceInfoGattBytes = 23;
  static const versionedAudioGattBytes = 62;
}

/// 固件命令码。
abstract final class BoardCommand {
  static const audioData = 0x01;
  static const consumer = 0x0C;
  static const status = 0x12;
  static const getDeviceInfo = 0x13;
  static const getKeyConfig = 0x15;
  static const setKeyConfig = 0x16;
  static const deviceDisconnect = 0x60;
  static const getSilentRecord = 0x61;
  static const setSilentRecord = 0x62;
  static const getSleepTimeout = 0x63;
  static const setSleepTimeout = 0x64;
  static const appOnlineNotify = 0x65;
  static const getAppOnline = 0x66;
  static const getOpenUrl = 0x67;
  static const setOpenUrl = 0x68;
  static const getAudioCapabilities = 0x6E;
  static const audioStreamControl = 0x6F;
  static const workModeData = 0xC9;
}
