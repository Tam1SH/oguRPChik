use crate::main_loop::run_session;
use crate::codecs::base::{MessageCodec, Envelope};
use anyhow::anyhow;
use dashmap::DashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::{error, info};
use crate::discovery::Topology;
use crate::service_handler::ServiceHandler;
use crate::transport::base::{BufferAllocator, MessageSink, MessageSource, Transport};

pub struct ClientPerCore<C, Si, So, A>
where
    C: MessageCodec,
    Si: MessageSink<Payload = C::Dest>,
{
    sink: Si,
    pending: Rc<DashMap<u64, oneshot::Sender<C::Dest>>>,
    next_id: Rc<AtomicU64>,
    _phantom: PhantomData<(C, So, A)>,
}

impl<C, Si, So, A> Clone for ClientPerCore<C, Si, So, A>
where
    C: MessageCodec,
    Si: MessageSink<Payload = C::Dest> + Clone,
{
    fn clone(&self) -> Self {
        Self {
            next_id: self.next_id.clone(),
            sink: self.sink.clone(),
            pending: self.pending.clone(),
            _phantom: PhantomData,
        }
    }
}


impl<C, Si, So, A> ClientPerCore<C, Si, So, A>
where
    C: MessageCodec + 'static,
    Si: MessageSink<Payload = C::Dest> + Clone + 'static,
    So: MessageSource<Payload = C::Dest> + 'static,
    A: BufferAllocator<Payload = C::Dest> + 'static,
{
    pub fn new(
        sink: Si,
        pending: Rc<DashMap<u64, oneshot::Sender<C::Dest>>>,
    ) -> Self {
        Self {
            sink,
            pending,
            next_id: Rc::new(AtomicU64::new(0)),
            _phantom: PhantomData,
        }
    }


    pub async fn connect<T: Transport<Si, So>>(
        transport: T,
    ) -> anyhow::Result<Self> {

        let (sink, source) = transport.decompose()?;

        let pending = Rc::new(DashMap::new());
        let p_clone = pending.clone();
        let sink_clone = sink.clone();

        compio::runtime::spawn(async move {
            run_session::<(C, A), _, _, _>(
                NoOpHandler,
                sink_clone,
                source,
                p_clone
            ).await;
        }).detach();

        Ok(Self { sink, pending, next_id: Rc::new(AtomicU64::new(0)), _phantom: PhantomData })
    }


    pub async fn call(&mut self, req: C::Request) -> anyhow::Result<ResponseGuard<C::Dest, C>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();

        self.pending.insert(id, tx);

        type CurrentProtocolMessageType<C> = Envelope<<C as MessageCodec>::Request, <C as MessageCodec>::Response>;
        let mut buf = A::allocate(size_of::<CurrentProtocolMessageType<C>>());

        C::encode(Envelope::Request { id, payload: req }, &mut buf)?;

        self.sink.send(buf).await?;

        let response_buf = match compio::time::timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(b)) => b,
            Ok(Err(_)) => {
                self.pending.remove(&id);
                return Err(anyhow!("Channel closed"));
            }
            Err(_) => {
                self.pending.remove(&id);
                return Err(anyhow!("Request timeout"));
            }
        };

        C::decode(response_buf.as_ref()).map_err(|e| anyhow!("Invalid response data: {e}"))?;

        Ok(ResponseGuard::try_new(response_buf)?)
    }
}

pub struct ResponseGuard<Payload: AsRef<[u8]> + Send + 'static, Codec: MessageCodec> {

    #[allow(unused)]
    payload_guard: Payload,

    view: Codec::ResponseView<'static>,
}

unsafe impl<Payload, Codec> Send for ResponseGuard<Payload, Codec>
where
    Payload: AsRef<[u8]> + Send + 'static,
    for<'b> Codec::ResponseView<'b>: Send,
    Codec: MessageCodec,
{}


impl<'a, Payload: AsRef<[u8]> + Send + 'static, Codec: MessageCodec> ResponseGuard<Payload, Codec> {
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

impl<Payload: AsRef<[u8]> + Send + 'static, Codec: MessageCodec> std::ops::Deref for ResponseGuard<Payload, Codec> {

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
