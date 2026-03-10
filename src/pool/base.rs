use crate::codecs::base::OwnedBuf;
use crate::transport::base::pool_config::PoolConfig;

pub trait PoolStrategy<B: OwnedBuf>: Clone + 'static {
    /// See [`crate::codecs::base::BufferAllocator`]
    type SendMark: 'static;

    fn get(config: &PoolConfig) -> Self;
    fn acquire(&self, cap: usize) -> B;
    fn release(&self, buf: B);
}
