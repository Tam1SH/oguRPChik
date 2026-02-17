use crate::align_buffer::AlignedBuffer;
use crate::message_codec::{Envelope, MessageCodec};
use anyhow::Result;
use rkyv::api::high::{to_bytes_in, HighSerializer, HighValidator};
use rkyv::bytecheck::CheckBytes;
use rkyv::rancor::Error;
use rkyv::ser::allocator::ArenaHandle;
use rkyv::util::AlignedVec;
use rkyv::{access, Archive, Serialize};
use crate::server::{DefaultVecAlloc, HasDefaultAllocator};
use crate::tpc_pool::TpcPool;
use crate::transport::base::BufferAllocator;

#[derive(Archive, Serialize)]
pub(crate) enum RkyvEnvelope<Req, Res> {
    Request { id: u64, payload: Req },
    Response { id: u64, payload: Res },
    Push { payload: Req },
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
            ArchivedRkyvEnvelope::Push { payload } => Ok(Envelope::Push { payload }),
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
            Envelope::Push { payload } => RkyvEnvelope::Push { payload },
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

#[derive(Clone)]
pub struct RkyvAllocator;

impl BufferAllocator for RkyvAllocator {
    type Payload = AlignedBuffer;
    fn allocate(size_hint: usize) -> Self::Payload {
        TpcPool::acquire_body(size_hint)
    }
}

impl HasDefaultAllocator for AlignedBuffer {
    type Alloc = RkyvAllocator;
}


