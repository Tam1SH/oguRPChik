use crate::align_buffer::AlignedBuffer;
use crate::message_codec::{Envelope, MessageCodec};
use crate::tpc_pool::TpcPool;
use crate::ServiceHandler;
use compio::buf::IoBuf;
use std::collections::HashMap;
use std::rc::Rc;

use dashmap::DashMap;
use std::sync::Arc;
use tracing::error;
use crate::transport::base::handle::{MsgBatch, PeerSink};
use crate::transport::base::{BufferAllocator, MessageSink, MessageSource, Transport};

pub trait SessionConfig {
    type Codec: MessageCodec<Dest = Self::Payload>;
    type Payload: AsRef<[u8]> + 'static;
    type Alloc: BufferAllocator<Payload = Self::Payload>;
}

impl<P, A, Pay> SessionConfig for (P, A)
where
    P: MessageCodec<Dest = Pay>,
    A: BufferAllocator<Payload = Pay>,
    Pay: AsRef<[u8]> + 'static,
{
    type Codec = P;
    type Payload = Pay;
    type Alloc = A;
}

pub async fn run_session<C, H, Sink, Source>(
    handler: H,
    sink: Sink,
    mut source: Source,
    pending: Rc<DashMap<u64, oneshot::Sender<C::Payload>>>,
)
where
    C: SessionConfig,
    H: ServiceHandler<C::Codec>,
    Sink: MessageSink<Payload = C::Payload>,
    Source: MessageSource<Payload = C::Payload>,
{
    while let Some(raw) = source.recv().await {

        match C::Codec::decode(raw.as_ref()) {
            Ok(Envelope::Request { id, payload }) => {
                if let Ok(resp) = handler.on_request(payload).await {

                    type Size<C: MessageCodec> = Envelope<C::Request, C::Response>;

                    let mut out_buf = C::Alloc::allocate(size_of::<Size<C::Codec>>());

                    if C::Codec::encode(Envelope::Response { id, payload: resp }, &mut out_buf).is_ok() {
                        let _ = sink.send(out_buf).await;
                    }
                }
            }
            Ok(Envelope::Push { payload }) => {

                let _ = handler.on_request(payload).await;
            }
            Ok(Envelope::Response { id, .. }) => {
                if let Some((_, tx)) = pending.remove(&id) {
                    let _ = tx.send(raw);
                }
            }
            Err(e) => {
                error!("Protocol decode error: {e}");
            }
        }
    }

}

