import 'package:flutter_test/flutter_test.dart';
import 'package:reai_board_sdk/reai_board_sdk.dart';
import 'package:reai_board_sdk_example/main.dart';

void main() {
  testWidgets('显示 SDK 扫描入口', (tester) async {
    await tester.pumpWidget(const BoardSdkExampleApp());

    expect(find.text('ReAI Board SDK 1.0.0 (2)'), findsOneWidget);
    expect(find.text('扫描 REAI_VB_ 设备'), findsOneWidget);
    expect(find.text('事件日志'), findsOneWidget);
  });

  test('SDK 事件被格式化为可读控制台日志', () {
    expect(
      formatBoardEventForLog(
        const BoardCapabilityWarningEvent('当前 GATT 有效载荷 20 字节'),
      ),
      'capability_warning message=当前 GATT 有效载荷 20 字节',
    );
    expect(
      formatBoardEventForLog(
        const KeyPressEvent(
          keyIndex: 6,
          keyName: 'AI语音(KEY6)',
          keyValue: 0x0F04,
          pressed: true,
        ),
      ),
      contains('key index=6'),
    );
  });
}
