use crate::codecs::base::{BorrowedBuf, BufferAllocator, Envelope, MessageCodec};
use crate::high::service_handler::ServiceHandler;
use crate::transport::base::{MessageSink, MessageSource, Transport};
use anyhow::anyhow;
use dashmap::DashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use crate::low::main_loop::run_session;

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub timeout_seconds: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 10,
        }
    }
}

pub struct ClientPerCore<C, Si, So, A, RxPayload>
where
    C: MessageCodec,
    Si: MessageSink<Payload = C::Dest>,
    A: BufferAllocator<Payload = C::Dest>,
{
    sink: Si,
    pending: Rc<DashMap<u64, oneshot::Sender<RxPayload>>>,
    next_id: Rc<AtomicU64>,
    config: ClientConfig,
    allocator: A,
    _phantom: PhantomData<(C, So, RxPayload)>,
}

impl<C, Si, So, A, RxPayload> Clone for ClientPerCore<C, Si, So, A, RxPayload>
where
    C: MessageCodec,
    Si: MessageSink<Payload = C::Dest>,
    A: BufferAllocator<Payload = C::Dest>,
{
    fn clone(&self) -> Self {
        Self {
            next_id: self.next_id.clone(),
            sink: self.sink.clone(),
            pending: self.pending.clone(),
            _phantom: PhantomData,
            config: self.config.clone(),
            allocator: self.allocator.clone(),
        }
    }
}

impl<C, Si, So, A, RxPayload> ClientPerCore<C, Si, So, A, RxPayload>
where
    C: MessageCodec,
    Si: MessageSink<Payload = C::Dest>,
    So: MessageSource<Payload = RxPayload>,
    A: BufferAllocator<Payload = C::Dest>,
    RxPayload: BorrowedBuf
{
    pub async fn connect<T: Transport<Si, So>>(
        transport: T,
        config: ClientConfig,
        allocator: A,
    ) -> anyhow::Result<Self> {
        let (sink, source) = transport.decompose()?;

        let pending = Rc::new(DashMap::new());
        let p_clone = pending.clone();
        let sink_clone = sink.clone();
        let a_clone = allocator.clone();
        compio::runtime::spawn(async move {
            run_session::<(C, A, RxPayload), _, _, _>(NoOpHandler, sink_clone, source, p_clone, a_clone).await;
        })
        .detach();

        Ok(Self {
            sink,
            pending,
            next_id: Rc::new(AtomicU64::new(0)),
            _phantom: PhantomData,
            config,
            allocator
        })
    }

    pub async fn call(&mut self, req: C::Request) -> anyhow::Result<ResponseGuard<RxPayload, C>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();

        self.pending.insert(id, tx);

        type CurrentProtocolMessageType<C> =
            Envelope<<C as MessageCodec>::Request, <C as MessageCodec>::Response>;

        let mut buf = self.allocator.allocate(size_of::<CurrentProtocolMessageType<C>>() * 2);

        C::encode(Envelope::Request { id, payload: req }, &mut buf)?;

        self.sink.send(buf).await?;

        let response_buf =
            match compio::time::timeout(Duration::from_secs(self.config.timeout_seconds), rx).await
            {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => {
                    self.pending.remove(&id);
                    return Err(anyhow!("Channel closed").context(e));
                }
                Err(_) => {
                    self.pending.remove(&id);
                    return Err(anyhow!(
                        "Request timeout ({}s)",
                        self.config.timeout_seconds
                    ));
                }
            };

        C::decode(response_buf.as_ref()).map_err(|e| anyhow!("Invalid response data: {e}"))?;

        Ok(ResponseGuard::try_new(response_buf)?)
    }
}

pub struct ResponseGuard<Payload: BorrowedBuf, Codec: MessageCodec> {
    #[allow(unused)]
    payload_guard: Payload,

    view: Codec::ResponseView<'static>,
}

unsafe impl<Payload, Codec> Send for ResponseGuard<Payload, Codec>
where
    Payload: BorrowedBuf,
    for<'b> Codec::ResponseView<'b>: Send,
    Codec: MessageCodec,
{
}

impl<'a, Payload: BorrowedBuf, Codec: MessageCodec> ResponseGuard<Payload, Codec> {
    pub fn try_new(payload: Payload) -> anyhow::Result<Self> {
        let (ptr, len) = {
            let slice = payload.as_ref();
            (slice.as_ptr(), slice.len())
        };

        let data_slice = unsafe { std::slice::from_raw_parts(ptr, len) };

        let decoded = Codec::decode(data_slice)?;

        let view = match decoded {
            Envelope::Response { payload: v, .. } => v,
            _ => return Err(anyhow::anyhow!("Not a response")),
        };

        let view_static = unsafe {
            std::mem::transmute::<Codec::ResponseView<'_>, Codec::ResponseView<'static>>(view)
        };

        Ok(Self {
            payload_guard: payload,
            view: view_static,
        })
    }
}

impl<Payload: BorrowedBuf, Codec: MessageCodec> std::ops::Deref
    for ResponseGuard<Payload, Codec>
{
    type Target = Codec::ResponseView<'static>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

#[derive(Clone)]
pub struct NoOpHandler;
impl<P: MessageCodec> ServiceHandler<P> for NoOpHandler {
    async fn on_request(
        &self,
        _: <P as MessageCodec>::RequestView<'_>,
    ) -> anyhow::Result<<P as MessageCodec>::Response> {
        Err(anyhow!("no op"))
    }
}
