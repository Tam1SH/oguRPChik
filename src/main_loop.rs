use crate::align_buffer::AlignedBuffer;
use crate::message_codec::{Envelope, MessageCodec};
use crate::tpc_pool::TpcPool;
use compio::buf::IoBuf;
use std::collections::HashMap;
use std::rc::Rc;

use dashmap::DashMap;
use std::sync::Arc;
use tracing::error;
use crate::service_handler::ServiceHandler;
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
    enum OnRequestAction<C: SessionConfig> {
        SendResponse { id: u64, resp: <<C as SessionConfig>::Codec as MessageCodec>::Response },
        DoNothing,
    }

    while let Some(raw) = source.recv().await {
        
        let response_id = match C::Codec::decode(raw.as_ref()) {
            Ok(Envelope::Response { id, .. }) => Some(id),
            _ => None,
        };

        if let Some(id) = response_id {
            if let Some((_, tx)) = pending.remove(&id) {
                
                let _ = tx.send(raw);
            }
            continue; 
        }

        let action = match C::Codec::decode(raw.as_ref()) {
            Ok(Envelope::Request { id, payload }) => {
                match handler.on_request(payload).await {
                    Ok(resp) => OnRequestAction::SendResponse { id, resp },
                    Err(_) => OnRequestAction::<C>::DoNothing,
                }
            }
            Ok(Envelope::Push { payload }) => {
                let _ = handler.on_request(payload).await;
                OnRequestAction::DoNothing
            }
            Err(e) => {
                error!("Protocol decode error: {e}");
                OnRequestAction::DoNothing
            }
            _ => unreachable!("Response handled above"),
        };

        
        if let OnRequestAction::SendResponse { id, resp } = action {
            
            type Env<C: MessageCodec> = Envelope<C::Request, C::Response>;
            let size_hint = size_of::<Env<C::Codec>>();

            let mut out_buf = C::Alloc::allocate(size_hint);

            if C::Codec::encode(Envelope::Response { id, payload: resp }, &mut out_buf).is_ok() {
                let _ = sink.send(out_buf).await;
            }
        }
    }
}
