use anyhow::Result;
use crate::transport::base::pool_config::PoolConfig;

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
impl<P: AsRef<[u8]> + Send + 'static> BorrowedBuf for P {}

pub trait OwnedBuf: BorrowedBuf + Sync + Default + Clone  {
    fn with_capacity(capacity: usize) -> Self;
    fn capacity(&self) -> usize;
    fn len(&self) -> usize;
    fn as_ptr(&self) -> *const u8;
    fn as_mut_ptr(&mut self) -> *mut u8;
    fn clear(&mut self);
}



pub trait HasAllocator {
    type Alloc: BufferAllocator<Payload = Self>;
    type SharedAlloc: BufferAllocator<Payload = Self, SendMark = ()>;
}


pub trait BufferAllocator: Send + Clone + 'static {
    type Payload: OwnedBuf;
    /// Controls whether this allocator (and types containing it) are [`Send`].
    ///
    /// Server workers are pinned to threads, so [`TpcAllocator`] can safely use
    /// thread-local pool storage without any synchronization. Clients, however, may
    /// run in work-stealing runtimes or be passed across threads, so [`SharedAllocator`]
    /// uses a global pool instead — avoiding both buffer leaks and abstraction bleed
    /// from forcing callers to manage per-thread pool lifetimes.
    type SendMark: 'static;
    fn get(config: &PoolConfig) -> Self;
    fn allocate(&self, size_hint: usize) -> Self::Payload;
    fn release(&self, buf: Self::Payload);
}


