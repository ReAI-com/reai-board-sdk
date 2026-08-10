//! 事件流门面 —— 包装 broadcast::Receiver,提供 async / 同步两种消费形态。
//!
//! [`EventStream`] 支持:
//! - `async fn recv()` —— async 消费(TUI / async 应用,可进 `tokio::select!`)
//! - `fn blocking_recv()` — sync blocking consumption (for dedicated threads
//!   that do not run a tokio runtime)
//!
//! 两种形态包装**同一个** `broadcast::Receiver`,零额外开销。
//! 溢出(broadcast 容量 256 满)用 [`EventStreamError::Lagged`] 显式暴露。

use tokio::sync::broadcast;

use crate::kernel::event::BoardEvent;

/// 事件流 —— 包装 `broadcast::Receiver<BoardEvent>`。
///
/// 由 [`BoardDevice::events()`](crate::facade::device::BoardDevice::events) 返回。同一设备可多次调 `events()`,
/// 各拿独立 receiver(底层 broadcast 保证多消费者互不抢)。
pub struct EventStream {
    rx: broadcast::Receiver<BoardEvent>,
}

impl EventStream {
    pub(crate) fn new(rx: broadcast::Receiver<BoardEvent>) -> Self {
        Self { rx }
    }

    /// 异步接收下一个事件。可在 `tokio::select!` 分支里用。
    ///
    /// 返回:
    /// - `Ok(Some(evt))` — 收到事件
    /// - `Ok(None)` — 通道关闭(所有 Sender drop)
    /// - `Err(EventStreamError::Lagged(n))` — 消费太慢,跳过 n 个事件
    pub async fn recv(&mut self) -> Result<Option<BoardEvent>, EventStreamError> {
        match self.rx.recv().await {
            Ok(evt) => Ok(Some(evt)),
            Err(broadcast::error::RecvError::Closed) => Ok(None),
            Err(broadcast::error::RecvError::Lagged(n)) => Err(EventStreamError::Lagged(n)),
        }
    }

    /// 同步阻塞接收下一个事件(std::thread 上下文用)。
    ///
    /// Return semantics match [`recv`](Self::recv). Useful for sync-context
    /// consumers that run on dedicated threads without a tokio runtime.
    pub fn blocking_recv(&mut self) -> Result<Option<BoardEvent>, EventStreamError> {
        match self.rx.blocking_recv() {
            Ok(evt) => Ok(Some(evt)),
            Err(broadcast::error::RecvError::Closed) => Ok(None),
            Err(broadcast::error::RecvError::Lagged(n)) => Err(EventStreamError::Lagged(n)),
        }
    }
}

/// 事件流错误
#[derive(Debug, thiserror::Error)]
pub enum EventStreamError {
    /// 消费太慢,broadcast 容量(256)满,跳过了 n 个事件。
    /// 非致命,继续 recv 可拿到后续事件。
    #[error("事件溢出,跳过 {0} 条")]
    Lagged(u64),
}
