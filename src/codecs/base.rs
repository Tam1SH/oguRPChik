use crate::pool::buf_guard::BufGuard;
use crate::pool::pool_stats::{PoolStats, PoolStatsSnapshot};
use crate::transport::base::pool_config::PoolConfig;
use anyhow::Result;
use std::sync::Arc;

pub enum Envelope<Req, Res> {
    Request { id: u64, payload: Req },
    Response { id: u64, payload: Res },
    Event { payload: Req },
}

pub trait MessageCodec: Send + Sync + 'static {
    type Request: Send + Sync + 'static;
    type Response: Send + Sync + 'static;

    type RequestView<'a>: Send + Sync;
    type ResponseView<'a>: Send + Sync;
    type Dest: OwnedBuf;

    fn decode(data: &[u8]) -> Result<Envelope<Self::RequestView<'_>, Self::ResponseView<'_>>>;

    fn encode(msg: Envelope<Self::Request, Self::Response>, dest: &mut Self::Dest) -> Result<()>;

    fn kind() -> &'static str;
}

pub trait BorrowedBuf: AsRef<[u8]> + Send + 'static {}
impl<B: AsRef<[u8]> + Send + 'static> BorrowedBuf for B {}

/// Marker: this buffer returns itself to the right place on drop.
/// Implementors are responsible for correct Drop behaviour.
pub trait ReleasableBuf: BorrowedBuf {}

pub trait OwnedBuf: BorrowedBuf + Clone {
    fn with_capacity(capacity: usize) -> Self;
    fn capacity(&self) -> usize;
    fn len(&self) -> usize;
    fn as_ptr(&self) -> *const u8;
    fn as_mut_ptr(&mut self) -> *mut u8;
    fn clear(&mut self);
}

pub trait HasAllocator: OwnedBuf {
    type Alloc: BufferAllocator<Payload = Self>;
    type SharedAlloc: BufferAllocator<Payload = Self, SendMark = ()>;
}

pub trait BufferAllocator: Send + Clone + 'static {
    type Payload: OwnedBuf;
    /// Controls whether this allocator (and types containing it) are [`Send`].
    ///
    /// Server workers are pinned to threads, so [`TpcAllocator`] can safely use
    /// thread-local pool storage without any synchronization. [`SharedAllocator`],
    /// however, uses a global pool — because response buffers may be dropped on a
    /// thread different from the one that allocated them (e.g. in work-stealing
    /// runtimes), so returning a buffer to a thread-local pool would either leak it
    /// or corrupt the wrong pool.
    type SendMark: 'static;
    fn get(config: &PoolConfig) -> Self;
    fn allocate(&self, size_hint: usize) -> Self::Payload;

    /// Allocate using only the EMA — no external hint required.
    /// Ideal call site after the pool has warmed up:
    fn allocate_hinted(&self) -> Self::Payload {
        self.allocate(0) // effective = max(0, ema) = ema
    }
    fn release(&self, buf: Self::Payload);
    fn stats(&self) -> Arc<PoolStats>;
}

pub trait AllocatorExt: BufferAllocator {
    fn stats_snapshot(&self) -> PoolStatsSnapshot {
        self.stats().snapshot()
    }
}
