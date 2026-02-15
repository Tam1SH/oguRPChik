use crate::align_buffer::AlignedBuffer;

use crate::main_loop::{run_session, SessionConfig};
use crate::message_codec::{MessageCodec, Envelope};
use crate::tpc_pool::TpcPool;
use anyhow::anyhow;
use dashmap::DashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use crate::ServiceHandler;
use crate::transport::base::handle::PeerSink;
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


    pub async fn connect<T: Transport<Si, So>>(transport: T) -> anyhow::Result<Self> {
        let (sink, source) = transport.decompose()?;
        let pending = Rc::new(DashMap::new());

        let cloned_sink = sink.clone();
        let p_clone = pending.clone();

        compio::runtime::spawn(async move {
            run_session::<(C, A), _, _, _>(NoOpHandler, cloned_sink, source, p_clone).await;
        })
        .detach();

        Ok(Self {
            sink,
            pending,
            next_id: Rc::new(AtomicU64::new(0)),
            _phantom: PhantomData,
        })
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

pub struct ResponseGuard<Payload: AsRef<[u8]> + Send, Codec: MessageCodec> {

    payload: Payload,

    view_ptr: *const Codec::ResponseView,
}

unsafe impl<Payload, Codec> Send for ResponseGuard<Payload, Codec>
    where
        Payload: AsRef<[u8]> + Send,
        Codec: MessageCodec
{}


impl<Payload: AsRef<[u8]> + Send, Codec: MessageCodec> ResponseGuard<Payload, Codec> {
    pub fn try_new(payload: Payload) -> anyhow::Result<Self> {

        let data_slice = payload.as_ref();

        let decoded = Codec::decode(data_slice)?;

        let view_ref = match decoded {
            Envelope::Response { payload, .. } => payload,
            _ => return Err(anyhow::anyhow!("Not a response")),
        };

        let view_ptr = view_ref as *const Codec::ResponseView;

        Ok(Self {
            payload,
            view_ptr,
        })
    }
}

impl<Payload: AsRef<[u8]> + Send, Codec: MessageCodec> std::ops::Deref for ResponseGuard<Payload, Codec> {
    type Target = Codec::ResponseView;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.view_ptr }
    }
}

#[derive(Clone)]
pub struct NoOpHandler;
impl<P: MessageCodec> ServiceHandler<P> for NoOpHandler {
    async fn on_request(
        &self,
        _: &<P as MessageCodec>::RequestView,
    ) -> anyhow::Result<<P as MessageCodec>::Response> {
        Err(anyhow!("no op"))
    }
}
