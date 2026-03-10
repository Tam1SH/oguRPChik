use crate::codecs::base::{BufferAllocator, OwnedBuf};
use crate::pool::base::PoolStrategy;
use crate::pool::pool_local::TpcStrategy;
use crate::pool::pool_shared::SharedStrategy;
use crate::transport::base::pool_config::PoolConfig;

#[derive(Clone)]
pub struct TpcAllocator<B: OwnedBuf>(TpcStrategy<B>);

impl<B: OwnedBuf> BufferAllocator for TpcAllocator<B> {
    type Payload = B;
    type SendMark = *const (); // !Send

    fn get(config: &PoolConfig) -> Self {
        Self(TpcStrategy::get(config))
    }
    fn allocate(&self, cap: usize) -> B { self.0.acquire(cap) }
    fn release(&self, buf: B) { self.0.release(buf) }
}

#[derive(Clone)]
pub struct SharedAllocator<B: OwnedBuf>(SharedStrategy<B>);

impl<B: OwnedBuf> BufferAllocator for SharedAllocator<B> {
    type Payload = B;
    type SendMark = (); // Send

    fn get(config: &PoolConfig) -> Self {
        Self(SharedStrategy::get(config))
    }
    fn allocate(&self, cap: usize) -> B { self.0.acquire(cap) }
    fn release(&self, buf: B) { self.0.release(buf) }
}