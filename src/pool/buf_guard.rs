use crate::codecs::base::{BufferAllocator, OwnedBuf, ReleasableBuf};

#[derive(Clone)]
pub struct BufGuard<B: OwnedBuf, A: BufferAllocator<Payload = B>> {
    buf: Option<B>,
    allocator: A,
}

impl<B: OwnedBuf, A: BufferAllocator<Payload = B>> BufGuard<B, A> {
    pub fn new(buf: B, allocator: A) -> Self {
        Self {
            buf: Some(buf),
            allocator,
        }
    }
}

impl<B: OwnedBuf, A: BufferAllocator<Payload = B>> OwnedBuf for BufGuard<B, A> {
    fn with_capacity(_: usize) -> Self {
        unreachable!()
    }
    fn capacity(&self) -> usize {
        unsafe { self.buf.as_ref().unwrap_unchecked().capacity() }
    }
    fn len(&self) -> usize {
        unsafe { self.buf.as_ref().unwrap_unchecked().len() }
    }
    fn as_ptr(&self) -> *const u8 {
        unsafe { self.buf.as_ref().unwrap_unchecked().as_ptr() }
    }
    fn as_mut_ptr(&mut self) -> *mut u8 {
        unsafe { self.buf.as_mut().unwrap_unchecked().as_mut_ptr() }
    }
    fn clear(&mut self) {
        unsafe { self.buf.as_mut().unwrap_unchecked().clear() }
    }
}
impl<B, A> AsRef<[u8]> for BufGuard<B, A>
where
    B: OwnedBuf,
    A: BufferAllocator<Payload = B>,
{
    fn as_ref(&self) -> &[u8] {
        self.buf.as_ref().unwrap().as_ref()
    }
}

impl<B: OwnedBuf, A: BufferAllocator<Payload = B>> ReleasableBuf for BufGuard<B, A> {}

impl<B, A> Drop for BufGuard<B, A>
where
    B: OwnedBuf,
    A: BufferAllocator<Payload = B>,
{
    fn drop(&mut self) {
        let buf = unsafe { self.buf.take().unwrap_unchecked() };
        self.allocator.release(buf);
    }
}
