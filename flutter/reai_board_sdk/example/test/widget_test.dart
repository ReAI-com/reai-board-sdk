import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:reai_board_sdk/reai_board_sdk.dart';
import 'package:reai_board_sdk_example/main.dart';

void main() {
  testWidgets('显示 SDK 扫描入口', (tester) async {
    await tester.pumpWidget(const BoardSdkExampleApp());

    expect(find.text('ReAI Board SDK 1.0.0 (3)'), findsOneWidget);
    expect(find.text('扫描 REAI_VB_ 设备'), findsOneWidget);
    expect(find.text('设备状态'), findsOneWidget);
    expect(find.text('硬件输入'), findsOneWidget);
    expect(find.text('旋钮'), findsOneWidget);
    expect(find.text('功能键'), findsOneWidget);
    expect(find.text('模式拨杆'), findsOneWidget);
    await tester.scrollUntilVisible(
      find.text('事件日志'),
      400,
      scrollable: find.byType(Scrollable).first,
    );
    expect(find.text('事件日志'), findsOneWidget);
  });

  testWidgets('硬件输入面板展示 12 键、当前绑定和按压状态', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: HardwareInputPanel(
              config: KeyConfig.factoryDefaults(),
              pressedKeys: const {6},
              eventCounts: const {6: 3},
              selectedMode: WorkMode.chat,
              selectedKeyIndex: 6,
              dirtyKeys: const {6},
              onRefresh: null,
              onSelectKey: (_) {},
              onBindingChanged: (_, _) {},
              onWrite: null,
              onRestoreDefaults: null,
            ),
          ),
        ),
      ),
    );

    expect(find.byKey(const ValueKey('hardware-key-0')), findsOneWidget);
    expect(find.byKey(const ValueKey('hardware-key-11')), findsOneWidget);
    expect(find.text('AI 语音'), findsWidgets);
    expect(find.text('按下 · 3 次'), findsOneWidget);
    expect(find.text('当前模式'), findsOneWidget);
    expect(find.text('CHAT'), findsWidgets);
    expect(find.textContaining('当前绑定 · KEY6'), findsOneWidget);
    expect(find.text('写入设备（1 项）'), findsOneWidget);
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
