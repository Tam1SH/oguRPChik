use crate::codecs::base::{
    BorrowedBuf, BufferAllocator, Envelope, MessageCodec, OwnedBuf, ReleasableBuf,
};
use crate::high::service_handler::ServiceHandler;
use crate::transport::base::{MessageSink, MessageSource};
use dashmap::DashMap;
use std::rc::Rc;
use tracing::{error, info};

pub trait SessionConfig {
    type TxAlloc: BufferAllocator;
    type Codec: MessageCodec<Dest = <Self::TxAlloc as BufferAllocator>::Payload>;
    type RxPayload: ReleasableBuf;
}

impl<P, TxA, RxPayload> SessionConfig for (P, TxA, RxPayload)
where
    P: MessageCodec<Dest = <TxA as BufferAllocator>::Payload>,
    TxA: BufferAllocator,
    RxPayload: ReleasableBuf,
{
    type TxAlloc = TxA;
    type Codec = P;
    type RxPayload = RxPayload;
}

pub async fn run_session<C, H, Sink, Source>(
    handler: H,
    sink: Sink,
    mut source: Source,
    pending: Rc<DashMap<u64, oneshot::Sender<C::RxPayload>>>,
    tx_allocator: C::TxAlloc,
) where
    C: SessionConfig,
    H: ServiceHandler<C::Codec>,
    Sink: MessageSink<Payload = <C::TxAlloc as BufferAllocator>::Payload>,
    Source: MessageSource<Payload = C::RxPayload>,
{
    enum OnRequestAction<C: SessionConfig> {
        SendResponse {
            id: u64,
            resp: <<C as SessionConfig>::Codec as MessageCodec>::Response,
        },
        DoNothing,
    }

    info!("entering main loop");

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
            Ok(Envelope::Request { id, payload }) => match handler.on_request(payload).await {
                Ok(resp) => OnRequestAction::SendResponse { id, resp },
                Err(_) => OnRequestAction::<C>::DoNothing,
            },
            Ok(Envelope::Event { payload }) => {
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
            let mut out_buf = tx_allocator.allocate_hinted();

            if C::Codec::encode(Envelope::Response { id, payload: resp }, &mut out_buf).is_ok() {
                let _ = sink.send(out_buf).await;
            }
        }
    }
}
