import 'package:flutter_test/flutter_test.dart';
import 'package:reai_board_sdk_example/main.dart';

void main() {
  testWidgets('显示 SDK 扫描入口', (tester) async {
    await tester.pumpWidget(const BoardSdkExampleApp());

    expect(find.text('扫描 REAI_VB_ 设备'), findsOneWidget);
    expect(find.text('事件日志'), findsOneWidget);
  });
}
