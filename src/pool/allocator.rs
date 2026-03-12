use crate::codecs::base::{BufferAllocator, OwnedBuf, ReleasableBuf};
use crate::pool::base::PoolStrategy;
use crate::pool::pool_local::TpcStrategy;
use crate::pool::pool_shared::SharedStrategy;
use crate::pool::pool_stats::{PoolStats, PoolStatsSnapshot};
use crate::transport::base::pool_config::PoolConfig;
use std::sync::Arc;

macro_rules! impl_allocator {
    ($name:ident, $strategy:ident, $send_mark:ty) => {
        #[derive(Clone)]
        pub struct $name<B: OwnedBuf>($strategy<B>);

        impl<B: OwnedBuf + ReleasableBuf> BufferAllocator for $name<B> {
            type Payload = B;
            type SendMark = $send_mark;

            fn get(config: &PoolConfig) -> Self {
                Self($strategy::get(config))
            }

            fn allocate(&self, size_hint: usize) -> Self::Payload {
                self.0.acquire(size_hint)
            }

            fn allocate_hinted(&self) -> Self::Payload {
                self.allocate(0)
            }

            fn release(&self, buf: Self::Payload) {
                self.0.release(buf);
            }

            fn stats(&self) -> Arc<PoolStats> {
                self.0.stats()
            }
        }
    };
}

impl_allocator!(TpcAllocator, TpcStrategy, *const ());
impl_allocator!(SharedAllocator, SharedStrategy, ());

pub trait AllocatorExt: BufferAllocator {
    fn stats_snapshot(&self) -> PoolStatsSnapshot {
        self.stats().snapshot()
    }
}

impl<A: BufferAllocator> AllocatorExt for A {}
