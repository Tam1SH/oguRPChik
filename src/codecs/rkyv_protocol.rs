use crate::codecs::base::{Envelope, HasAllocator, MessageCodec, OwnedBuf, ReleasableBuf};
use crate::pool::allocator::{SharedAllocator, TpcAllocator};
use anyhow::Result;
use compio::buf::{IoBuf, IoBufMut, SetLen};
use rkyv::api::high::{HighSerializer, HighValidator, to_bytes_in};
use rkyv::bytecheck::CheckBytes;
use rkyv::rancor::Error;
use rkyv::ser::allocator::ArenaHandle;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Serialize, access};
use std::mem::MaybeUninit;

#[derive(Archive, Serialize)]
pub(crate) enum RkyvEnvelope<Req, Res> {
    Request { id: u64, payload: Req },
    Response { id: u64, payload: Res },
    Event { payload: Req },
}

pub trait SerializeBounds:
    Archive
    + Send
    + Sync
    + 'static
    + for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, Error>>
{
}

impl<T> SerializeBounds for T where
    T: Archive
        + Send
        + Sync
        + 'static
        + for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, Error>>
{
}

pub trait ArchivedBounds:
    for<'a> CheckBytes<HighValidator<'a, Error>> + Send + Sync + 'static
{
}

impl<T> ArchivedBounds for T where
    T: for<'a> CheckBytes<HighValidator<'a, Error>> + Send + Sync + 'static
{
}

#[derive(Clone)]
pub struct RkyvCodec<Req, Res> {
    _phantom: std::marker::PhantomData<(Req, Res)>,
}

impl<Req, Res> MessageCodec for RkyvCodec<Req, Res>
where
    Req: SerializeBounds,
    for<'a> Req::Archived: ArchivedBounds,
    for<'a> Res: SerializeBounds,
    for<'a> Res::Archived: ArchivedBounds,
    RkyvEnvelope<Req, Res>: Archive,
    for<'a> <RkyvEnvelope<Req, Res> as Archive>::Archived: CheckBytes<HighValidator<'a, Error>>,
    for<'a> RkyvEnvelope<Req, Res>: Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, Error>>,
{
    type Request = Req;
    type Response = Res;

    type RequestView<'a> = &'a Req::Archived;
    type ResponseView<'a> = &'a Res::Archived;

    type Dest = AlignedBuffer;

    fn decode(data: &[u8]) -> Result<Envelope<Self::RequestView<'_>, Self::ResponseView<'_>>> {
        let archived = access::<ArchivedRkyvEnvelope<Req, Res>, Error>(data)?;

        match archived {
            ArchivedRkyvEnvelope::Request { id, payload } => Ok(Envelope::Request {
                id: u64::from(id),
                payload,
            }),
            ArchivedRkyvEnvelope::Response { id, payload } => Ok(Envelope::Response {
                id: u64::from(*id),
                payload,
            }),
            ArchivedRkyvEnvelope::Event { payload } => Ok(Envelope::Event { payload }),
        }
    }

    fn encode(
        msg: Envelope<Self::Request, Self::Response>,
        dest: &mut AlignedBuffer,
    ) -> Result<()> {
        dest.0.clear();

        let envelope = match msg {
            Envelope::Request { id, payload } => RkyvEnvelope::Request { id, payload },
            Envelope::Response { id, payload } => RkyvEnvelope::Response { id, payload },
            Envelope::Event { payload } => RkyvEnvelope::Event { payload },
        };

        let writer = std::mem::take(&mut dest.0);

        let bytes = to_bytes_in::<_, Error>(&envelope, writer)?;

        dest.0 = bytes;

        Ok(())
    }

    fn kind() -> &'static str {
        "rkyv"
    }
}

#[derive(Default, Clone)]
pub struct AlignedBuffer(pub AlignedVec);

impl AlignedBuffer {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn with_capacity(capacity: usize) -> Self {
        Self(AlignedVec::with_capacity(capacity))
    }
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr()
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr()
    }
}

impl AsRef<[u8]> for AlignedBuffer {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl OwnedBuf for AlignedBuffer {
    fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity(capacity)
    }

    fn capacity(&self) -> usize {
        Self::capacity(self)
    }
    fn len(&self) -> usize {
        Self::len(self)
    }
    fn as_ptr(&self) -> *const u8 {
        Self::as_ptr(self)
    }
    fn as_mut_ptr(&mut self) -> *mut u8 {
        Self::as_mut_ptr(self)
    }

    fn clear(&mut self) {
        Self::clear(self);
    }
}

impl ReleasableBuf for AlignedBuffer {}

impl HasAllocator for AlignedBuffer {
    type Alloc = TpcAllocator<AlignedBuffer>;
    type SharedAlloc = SharedAllocator<AlignedBuffer>;
}
