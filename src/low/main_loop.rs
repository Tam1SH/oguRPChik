use crate::codecs::base::{BufferAllocator, OwnedBuf, Envelope, MessageCodec, BorrowedBuf};
use std::rc::Rc;
use crate::high::service_handler::ServiceHandler;
use crate::transport::base::{MessageSink, MessageSource};
use dashmap::DashMap;
use tracing::{error, info};

pub trait SessionConfig {
    type Codec: MessageCodec<Dest = Self::OutPayload>;
    type RxPayload: BorrowedBuf;
    type OutPayload: OwnedBuf;
    type Alloc: BufferAllocator<Payload = Self::OutPayload>;
}

impl<P, A, OutPayload, RxPayload> SessionConfig for (P, A, RxPayload)
where
    P: MessageCodec<Dest = OutPayload>,
    A: BufferAllocator<Payload = OutPayload>,
    OutPayload: OwnedBuf,
    RxPayload: BorrowedBuf,
{
    type Codec = P;
    type RxPayload = RxPayload;
    type OutPayload = OutPayload;
    type Alloc = A;
}

pub async fn run_session<C, H, Sink, Source>(
    handler: H,
    sink: Sink,
    mut source: Source,
    pending: Rc<DashMap<u64, oneshot::Sender<C::RxPayload>>>,
    allocator: C::Alloc,
) where
    C: SessionConfig,
    H: ServiceHandler<C::Codec>,
    Sink: MessageSink<Payload = C::OutPayload>,
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
            #[allow(type_alias_bounds)]
            type Env<C: MessageCodec> = Envelope<C::Request, C::Response>;

            let size_hint = size_of::<Env<C::Codec>>() * 2;

            let mut out_buf = allocator.allocate(size_hint);

            if C::Codec::encode(Envelope::Response { id, payload: resp }, &mut out_buf).is_ok() {
                let _ = sink.send(out_buf).await;
            }
        }
    }
}
