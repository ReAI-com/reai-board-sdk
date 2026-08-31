import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:reai_board_sdk/reai_board_sdk.dart';

void main() {
  test('按 3 字节小端格式解析 20 个按键槽位', () {
    final bytes = Uint8List(KeyConfig.byteLength);
    bytes.setAll(0, const [BoardKeyClass.media, 0x34, 0x12]);
    bytes.setAll(57, const [BoardKeyClass.keyboard, 0x06, 0x02]);

    final config = KeyConfig(bytes);

    expect(config.bindings, hasLength(KeyConfig.keyCount));
    expect(
      config.bindings.first,
      const KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x1234),
    );
    expect(
      config.bindings.last,
      const KeyBinding(keyClass: BoardKeyClass.keyboard, keyValue: 0x0206),
    );
    expect(config.activeBindings, hasLength(KeyConfig.activeKeyCount));
  });

  test('只序列化 12 个有效键并清零保留槽位', () {
    final bindings = List<KeyBinding>.generate(
      KeyConfig.activeKeyCount,
      (index) =>
          KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F01 + index),
    );

    final config = KeyConfig.fromActiveBindings(bindings);

    expect(config.bytes.sublist(36), everyElement(0));
    expect(config.activeBindings, bindings);
  });

  test('出厂映射和硬件 12 键定义与固件一致', () {
    final config = KeyConfig.factoryDefaults();

    expect(BoardPhysicalKey.values, hasLength(12));
    expect(BoardPhysicalKey.values.first.name, '旋钮左旋');
    expect(BoardPhysicalKey.values.last.name, 'CHAT 拨杆');
    expect(config.activeBindings.map((binding) => binding.keyValue), const [
      0x0F07,
      0x0F08,
      0x0F09,
      0x0F01,
      0x0F02,
      0x0F03,
      0x0F04,
      0x0F05,
      0x0F06,
      0x0F0A,
      0x0F0B,
      0x0F0C,
    ]);
  });

  test('按键绑定提供适合验收页展示的可读描述', () {
    expect(
      const KeyBinding(
        keyClass: BoardKeyClass.media,
        keyValue: 0x0F04,
      ).description,
      'AI 语音',
    );
    expect(
      const KeyBinding(
        keyClass: BoardKeyClass.keyboard,
        keyValue: 0x0206,
      ).description,
      'Shift L + C',
    );
    expect(
      const KeyBinding(
        keyClass: BoardKeyClass.disabled,
        keyValue: 0,
      ).description,
      '禁用',
    );
  });

  test('只替换指定有效键并原样保留其他及未知槽位', () {
    final bytes = Uint8List.fromList(
      List<int>.generate(KeyConfig.byteLength, (index) => index),
    );
    final original = KeyConfig(bytes);

    final updated = original.copyWithActiveBinding(
      6,
      const KeyBinding(keyClass: BoardKeyClass.disabled, keyValue: 0),
    );

    expect(updated.activeBindings[6].description, '禁用');
    expect(updated.bytes.sublist(0, 18), original.bytes.sublist(0, 18));
    expect(updated.bytes.sublist(21), original.bytes.sublist(21));
  });
}
