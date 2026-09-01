/// Flutter SDK 错误基类。
sealed class BoardException implements Exception {
  const BoardException(this.message);

  final String message;

  @override
  String toString() => '$runtimeType: $message';
}

final class BoardTransportException extends BoardException {
  const BoardTransportException(super.message, {this.cause});

  final Object? cause;
}

final class BoardProtocolException extends BoardException {
  const BoardProtocolException(super.message);
}

final class BoardTimeoutException extends BoardException {
  const BoardTimeoutException(super.message);
}

final class BoardDisconnectedException extends BoardException {
  const BoardDisconnectedException([super.message = 'ReAI-Vibe-Board 未连接']);
}

final class BoardUnsupportedException extends BoardException {
  const BoardUnsupportedException(super.message);
}

final class BoardMtuException extends BoardException {
  const BoardMtuException({
    required this.requiredBytes,
    required this.actualBytes,
  }) : super('GATT 有效载荷不足：需要 $requiredBytes 字节，当前 $actualBytes 字节');

  final int requiredBytes;
  final int actualBytes;
}
