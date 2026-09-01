import 'package:flutter_test/flutter_test.dart';
import 'package:reai_board_sdk/src/key_state.dart';
import 'package:reai_board_sdk/src/events.dart';

List<(int, bool)> keyChanges(List<BoardEvent> events) => events
    .whereType<KeyPressEvent>()
    .map((event) => (event.keyIndex, event.pressed))
    .toList();

void main() {
  test('旋钮脉冲立即成对按下和释放', () {
    final interpreter = BoardInputInterpreter();
    expect(keyChanges(interpreter.handleConsumer(0x0F07, Duration.zero)), [
      (0, true),
      (0, false),
    ]);
    expect(
      keyChanges(
        interpreter.handleConsumer(0, const Duration(milliseconds: 2)),
      ),
      isEmpty,
    );
  });

  test('按住 Tab 转旋钮后仍能收到 Tab 释放', () {
    final interpreter = BoardInputInterpreter();
    expect(keyChanges(interpreter.handleConsumer(0x0F01, Duration.zero)), [
      (3, true),
    ]);
    expect(
      keyChanges(
        interpreter.handleConsumer(0x0F08, const Duration(milliseconds: 10)),
      ),
      [(1, true), (1, false)],
    );
    expect(
      keyChanges(
        interpreter.handleConsumer(0, const Duration(milliseconds: 12)),
      ),
      isEmpty,
    );
    expect(
      keyChanges(
        interpreter.handleConsumer(0, const Duration(milliseconds: 300)),
      ),
      [(3, false)],
    );
  });

  test('按住 AI 语音键转旋钮不会误报释放', () {
    final interpreter = BoardInputInterpreter();
    final press = interpreter.handleConsumer(0x0F04, Duration.zero);
    expect(press.whereType<AiVoiceKeyEvent>().single.pressed, isTrue);

    final knob = interpreter.handleConsumer(
      0x0F08,
      const Duration(milliseconds: 10),
    );
    final tail = interpreter.handleConsumer(
      0,
      const Duration(milliseconds: 12),
    );
    expect([...knob, ...tail].whereType<AiVoiceKeyEvent>(), isEmpty);

    final release = interpreter.handleConsumer(
      0,
      const Duration(milliseconds: 300),
    );
    expect(release.whereType<AiVoiceKeyEvent>().single.pressed, isFalse);
  });

  test('断连补齐全部按键与语音键释放', () {
    final interpreter = BoardInputInterpreter();
    interpreter.handleConsumer(0x0F01, Duration.zero);
    interpreter.handleConsumer(0x0F04, const Duration(milliseconds: 10));
    final events = interpreter.releaseAll();
    expect(
      keyChanges(events),
      containsAll(<(int, bool)>[(3, false), (6, false)]),
    );
    expect(events.whereType<AiVoiceKeyEvent>().single.pressed, isFalse);
  });
}
