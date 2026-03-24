use crate::codecs::base::{Envelope, HasAllocator, MessageCodec, OwnedBuf, ReleasableBuf};
use crate::codecs::serde_compatible::serde_format::SerdeFormat;
use crate::pool::allocator::{SharedAllocator, TpcAllocator};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;

#[derive(Serialize, Deserialize)]
enum WireEnvelope<Req, Res> {
    Request { id: u64, payload: Req },
    Response { id: u64, payload: Res },
    Event { payload: Req },
}

pub struct SerdeProtocol<Req, Res, F> {
    _phantom: PhantomData<(Req, Res, F)>,
}

impl<Req, Res, F> MessageCodec for SerdeProtocol<Req, Res, F>
where
    F: SerdeFormat,
    Req: Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static,
    Res: Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static,
{
    type Request = Req;
    type Response = Res;

    type RequestView<'a> = Req;
    type ResponseView<'a> = Res;

    type Dest = VecBuf;

    fn decode(
        data: &[u8],
    ) -> anyhow::Result<Envelope<Self::RequestView<'_>, Self::ResponseView<'_>>> {
        let wire: WireEnvelope<Req, Res> = F::deserialize(data)?;

        match wire {
            WireEnvelope::Request { id, payload } => Ok(Envelope::Request { id, payload }),
            WireEnvelope::Response { id, payload } => Ok(Envelope::Response { id, payload }),
            WireEnvelope::Event { payload } => Ok(Envelope::Event { payload }),
        }
    }

    fn encode(
        msg: Envelope<Self::Request, Self::Response>,
        dest: &mut Self::Dest,
    ) -> anyhow::Result<()> {
        dest.clear();

        let wire = match msg {
            Envelope::Request { id, payload } => WireEnvelope::Request { id, payload },
            Envelope::Response { id, payload } => WireEnvelope::Response { id, payload },
            Envelope::Event { payload } => WireEnvelope::Event { payload },
        };

        F::serialize(&wire, &mut dest.0)
    }

    fn kind() -> &'static str {
        F::name()
    }
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct VecBuf(pub Vec<u8>);

impl AsRef<[u8]> for VecBuf {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl OwnedBuf for VecBuf {
    fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr()
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr()
    }

    fn clear(&mut self) {
        self.0.clear()
    }

    fn write_from_slice(&mut self, bytes: &[u8]) {
        self.0.clear();
        self.0.extend_from_slice(bytes);
    }
}

impl ReleasableBuf for VecBuf {}

impl HasAllocator for VecBuf {
    type Alloc = TpcAllocator<VecBuf>;
    type SharedAlloc = SharedAllocator<VecBuf>;
}
