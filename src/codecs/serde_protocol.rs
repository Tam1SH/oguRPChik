use std::marker::PhantomData;
use serde::{Deserialize, Serialize};
use crate::message_codec::{Envelope, MessageCodec};
use crate::align_buffer::AlignedBuffer;
use crate::codecs::JsonHandshake;
use crate::codecs::serde_compatible::serde_format::SerdeFormat;

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

    type Dest = AlignedBuffer;
    type Handshake = JsonHandshake;

    fn decode(data: &[u8]) -> anyhow::Result<Envelope<Self::RequestView<'_>, Self::ResponseView<'_>>> {
        let wire: WireEnvelope<Req, Res> = F::deserialize(data)?;

        match wire {
            WireEnvelope::Request { id, payload } => Ok(Envelope::Request { id, payload }),
            WireEnvelope::Response { id, payload } => Ok(Envelope::Response { id, payload }),
            WireEnvelope::Event { payload } => Ok(Envelope::Event { payload }),
        }
    }

    fn encode(msg: Envelope<Self::Request, Self::Response>, dest: &mut Self::Dest) -> anyhow::Result<()> {
        dest.0.clear();

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