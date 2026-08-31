/// 可注入的单调时钟和延时器，避免状态机测试依赖真实 sleep。
abstract interface class BoardClock {
  Duration get now;

  Future<void> delay(Duration duration);
}

final class SystemBoardClock implements BoardClock {
  SystemBoardClock() : _stopwatch = Stopwatch()..start();

  final Stopwatch _stopwatch;

  @override
  Duration get now => _stopwatch.elapsed;

  @override
  Future<void> delay(Duration duration) => Future<void>.delayed(duration);
}
