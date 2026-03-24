use std::cell::RefCell;
use crate::codecs::base::{BufferAllocator, Envelope, MessageCodec, ReleasableBuf};
use crate::high::service_handler::ServiceHandler;
use crate::low::handshake::{HandshakeMode, authenticate_client};
use crate::low::main_loop::run_session;
use crate::transport::base::{MessageSink, MessageSource, Transport};
use anyhow::anyhow;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use rustc_hash::FxHashMap;
use local_sync::oneshot;
#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub timeout_seconds: u64,
    pub handshake: HandshakeMode,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 10,
            handshake: HandshakeMode::Disabled,
        }
    }
}

pub struct ClientPerCore<C, Si, So, TxA, Rx>
where
    C: MessageCodec<Dest = TxA::Payload>,
    Si: MessageSink<Payload = TxA::Payload>,
    TxA: BufferAllocator,
    Rx: ReleasableBuf,
{
    sink: Si,
    pending: Rc<RefCell<FxHashMap<u64, oneshot::Sender<Rx>>>>,
    next_id: Rc<AtomicU64>,
    config: ClientConfig,
    tx_allocator: TxA,
    _phantom: PhantomData<(C, So)>,
}

impl<C, Si, So, TxA, Rx> Clone for ClientPerCore<C, Si, So, TxA, Rx>
where
    C: MessageCodec<Dest = TxA::Payload>,
    Si: MessageSink<Payload = TxA::Payload>,
    TxA: BufferAllocator,
    Rx: ReleasableBuf,
{
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
            pending: self.pending.clone(),
            next_id: self.next_id.clone(),
            config: self.config.clone(),
            tx_allocator: self.tx_allocator.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<C, Si, So, TxA, Rx> ClientPerCore<C, Si, So, TxA, Rx>
where
    C: MessageCodec<Dest = TxA::Payload>,
    Si: MessageSink<Payload = TxA::Payload>,
    So: MessageSource<Payload = Rx>,
    TxA: BufferAllocator,
    Rx: ReleasableBuf,
{
    pub async fn connect<T: Transport<Si, So>>(
        transport: T,
        config: ClientConfig,
        tx_allocator: TxA,
    ) -> anyhow::Result<Self> {
        let (sink, mut source) = transport.decompose()?;
        authenticate_client(&sink, &mut source, &tx_allocator, &config.handshake).await?;

        let pending = Rc::new(RefCell::new(FxHashMap::default()));
        let p_clone = pending.clone();
        let si_clone = sink.clone();
        let tx_clone = tx_allocator.clone();

        compio::runtime::spawn(async move {
            run_session::<(C, TxA, Rx), _, _, _>(NoOpHandler, si_clone, source, p_clone, tx_clone)
                .await;
        })
        .detach();

        Ok(Self {
            sink,
            pending,
            next_id: Rc::new(AtomicU64::new(0)),
            config,
            tx_allocator,
            _phantom: PhantomData,
        })
    }

    pub async fn call(&mut self, req: C::Request) -> anyhow::Result<ResponseGuard<Rx, C>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().insert(id, tx);

        let mut buf = self.tx_allocator.allocate_hinted();
        C::encode(Envelope::Request { id, payload: req }, &mut buf)?;
        self.sink.send(buf).await?;

        let response_buf =
            match compio::time::timeout(Duration::from_secs(self.config.timeout_seconds), rx).await
            {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => {
                    self.pending.borrow_mut().remove(&id);
                    return Err(anyhow!("Channel closed").context(e));
                }
                Err(_) => {
                    self.pending.borrow_mut().remove(&id);
                    return Err(anyhow!(
                        "Request timeout ({}s)",
                        self.config.timeout_seconds
                    ));
                }
            };

        ResponseGuard::try_new(response_buf)
    }
}

pub struct ResponseGuard<Rx: ReleasableBuf, Codec: MessageCodec> {
    #[allow(unused)]
    payload: Option<Rx>,
    view: Codec::ResponseView<'static>,
}

unsafe impl<Rx, Codec> Send for ResponseGuard<Rx, Codec>
where
    Rx: ReleasableBuf + Send,
    Codec: MessageCodec,
    for<'a> Codec::ResponseView<'a>: Send,
{
}

impl<Rx: ReleasableBuf, Codec: MessageCodec> ResponseGuard<Rx, Codec> {
    pub fn try_new(payload: Rx) -> anyhow::Result<Self> {
        let (ptr, len) = {
            let s = payload.as_ref();
            (s.as_ptr(), s.len())
        };

        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        let decoded = Codec::decode(slice).map_err(|e| anyhow!("Invalid response data: {e}"))?;

        let view = match decoded {
            Envelope::Response { payload: v, .. } => v,
            _ => return Err(anyhow!("Not a response")),
        };

        let view_static = unsafe {
            std::mem::transmute::<Codec::ResponseView<'_>, Codec::ResponseView<'static>>(view)
        };

        Ok(Self {
            payload: Some(payload),
            view: view_static,
        })
    }
}

impl<Rx: ReleasableBuf, Codec: MessageCodec> std::ops::Deref for ResponseGuard<Rx, Codec> {
    type Target = Codec::ResponseView<'static>;
    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

#[derive(Clone)]
pub struct NoOpHandler;

impl<P: MessageCodec> ServiceHandler<P> for NoOpHandler {
    async fn on_request(&self, _: P::RequestView<'_>) -> anyhow::Result<P::Response> {
        Err(anyhow!("no op"))
    }
}
