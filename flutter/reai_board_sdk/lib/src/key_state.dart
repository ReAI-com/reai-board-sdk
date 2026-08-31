import 'events.dart';
import 'models.dart';

const _pulseTailWindow = Duration(milliseconds: 100);

const _keyValues = <int>[
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
];

const _keyNames = <String>[
  '音量A相(KEY0)',
  '音量B相(KEY1)',
  '音量按压(KEY2)',
  'Tab键(KEY3)',
  'New键(KEY4)',
  'Esc键(KEY5)',
  'AI语音(KEY6)',
  'Action键(KEY7)',
  'Enter键(KEY8)',
  'YOLO拨杆(KEY9)',
  'PLAN拨杆(KEY10)',
  'CHAT拨杆(KEY11)',
];

/// 还原 Consumer 单值流中的按住集合、旋钮脉冲和模式拨杆。
final class BoardInputInterpreter {
  final List<int> _held = [];
  List<int> _previous = [];
  Duration? _lastKnobPulseAt;
  bool _aiVoicePressed = false;
  int? _modeEndpoint;

  List<BoardEvent> handleConsumer(int keyValue, Duration now) {
    final events = <BoardEvent>[];
    if (keyValue == 0) {
      final pulseAt = _lastKnobPulseAt;
      if (pulseAt != null && now - pulseAt <= _pulseTailWindow) {
        _lastKnobPulseAt = null;
        return events;
      }
      _held.clear();
      events.addAll(_applySnapshot(const []));
      if (_aiVoicePressed) {
        _aiVoicePressed = false;
        events.add(const AiVoiceKeyEvent(false));
      }
      if (_modeEndpoint != null) {
        _modeEndpoint = null;
        events.add(const ModeChangeEvent(WorkMode.chat));
      }
      return events;
    }

    final keyIndex = _keyValues.indexOf(keyValue);
    if (keyIndex < 0) return events;

    if (keyIndex <= 1) {
      _lastKnobPulseAt = now;
      events.addAll(_applySnapshot([..._held, keyIndex]));
      events.addAll(_applySnapshot([..._held]));
      return events;
    }

    _lastKnobPulseAt = null;
    if (!_held.contains(keyIndex)) _held.add(keyIndex);
    events.addAll(_applySnapshot([..._held]));

    if (keyIndex == 6 && !_aiVoicePressed) {
      _aiVoicePressed = true;
      events.add(const AiVoiceKeyEvent(true));
    }
    if (keyIndex == 9 || keyIndex == 10) {
      _modeEndpoint = keyIndex;
      events.add(
        ModeChangeEvent(keyIndex == 9 ? WorkMode.yolo : WorkMode.plan),
      );
    } else if (keyIndex == 11) {
      _modeEndpoint = null;
      events.add(const ModeChangeEvent(WorkMode.chat));
    }
    return events;
  }

  List<BoardEvent> releaseAll() {
    final events = <BoardEvent>[..._applySnapshot(const [])];
    _held.clear();
    _lastKnobPulseAt = null;
    if (_aiVoicePressed) {
      _aiVoicePressed = false;
      events.add(const AiVoiceKeyEvent(false));
    }
    if (_modeEndpoint != null) {
      _modeEndpoint = null;
      events.add(const ModeChangeEvent(WorkMode.chat));
    }
    return events;
  }

  List<BoardEvent> _applySnapshot(List<int> next) {
    final events = <BoardEvent>[];
    for (final index in _previous.where((index) => !next.contains(index))) {
      events.add(
        KeyPressEvent(
          keyIndex: index,
          keyName: _keyNames[index],
          keyValue: 0,
          pressed: false,
        ),
      );
    }
    for (final index in next.where((index) => !_previous.contains(index))) {
      events.add(
        KeyPressEvent(
          keyIndex: index,
          keyName: _keyNames[index],
          keyValue: _keyValues[index],
          pressed: true,
        ),
      );
    }
    if (next.length > 1 && next.any((index) => !_previous.contains(index))) {
      events.add(ComboKeyEvent(List<int>.unmodifiable(next)));
    }
    _previous = [...next];
    return events;
  }
}
