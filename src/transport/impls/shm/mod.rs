mod reactor;

use crate::codecs::base::{BorrowedBuf, OwnedBuf, ReleasableBuf};
use crate::transport::base::{
    MessageSink, MessageSource, RawMessageSink, RawMessageSource, TopologyRegistry, Transport,
    TransportAcceptor, TransportConnector, TransportPerWorkerBuilder,
};
use crate::transport::impls::shm::reactor::GlobalReactor;
use anyhow::Error;
use compio::buf::SetLen;
use iceoryx2::node::{Node, NodeBuilder};
use iceoryx2::port::LoanError;
use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::{WaitSet, WaitSetAttachmentId, WaitSetGuard, ipc};
use iceoryx2::sample::Sample;
use iceoryx2::service::ipc_threadsafe::Service;
use iceoryx2::service::port_factory::publish_subscribe::PortFactory;
use iceoryx2::waitset::WaitSetBuilder;
use std::cell::RefCell;
use std::future::poll_fn;
use std::io;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use tracing::{debug, error, info, trace, warn};

pub struct IceoryxPayload(Sample<Service, [u8], ()>);
impl ReleasableBuf for IceoryxPayload {}

impl AsRef<[u8]> for IceoryxPayload {
    fn as_ref(&self) -> &[u8] {
        self.0.payload()
    }
}

fn port_service_factory(
    node: &Node<Service>,
    name: &str,
) -> Result<PortFactory<Service, [u8], ()>, Error> {
    node.service_builder(&name.try_into()?)
        .publish_subscribe::<[u8]>()
        .max_publishers(4)
        .max_subscribers(4)
        .open_or_create()
        .map_err(|e| anyhow::anyhow!("Failed to open service {}: {:?}", name, e))
}

fn port_event_factory(
    node: &Node<Service>,
    base_name: &str,
) -> Result<iceoryx2::service::port_factory::event::PortFactory<Service>, Error> {
    let ev_name = format!("{}_events", base_name);
    node.service_builder(&ev_name.as_str().try_into()?)
        .event()
        .max_notifiers(4)
        .max_listeners(4)
        .open_or_create()
        .map_err(|e| anyhow::anyhow!("Failed to open event service {}: {:?}", ev_name, e))
}

pub struct IceoryxConnector<B> {
    service_name: String,
    phantom_data: PhantomData<B>,
}

impl<B: OwnedBuf> IceoryxConnector<B> {
    pub fn new(service_name: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
            phantom_data: PhantomData,
        }
    }
}

impl<B: OwnedBuf> TransportConnector<IceoryxSinkAdapter<B>, IceoryxSourceAdapter>
    for IceoryxConnector<B>
{
    type Transport = IceoryxTransport;

    async fn connect(&self) -> anyhow::Result<Self::Transport> {
        debug!(service = %self.service_name, "IceoryxConnector: connecting to service");

        let node = NodeBuilder::new()
            .create::<Service>()
            .map_err(|e| anyhow::anyhow!("Iceoryx Node error: {:?}", e))?;

        let c2s_name = format!("{}_c2s", self.service_name);
        let s2c_name = format!("{}_s2c", self.service_name);

        let svc_c2s = port_service_factory(&node, &c2s_name)?;
        let publisher = svc_c2s
            .publisher_builder()
            .initial_max_slice_len(1024 * 1024)
            .max_loaned_samples(16)
            .create()?;

        let svc_evt_c2s = port_event_factory(&node, &c2s_name)?;
        let notifier = svc_evt_c2s.notifier_builder().create()?;

        let svc_s2c = port_service_factory(&node, &s2c_name)?;
        let subscriber = svc_s2c.subscriber_builder().create()?;

        let svc_evt_s2c = port_event_factory(&node, &s2c_name)?;
        let listener = svc_evt_s2c.listener_builder().create()?;

        Ok(IceoryxTransport {
            publisher,
            subscriber,
            notifier,
            listener,
            _node: node,
        })
    }
}

pub struct IceoryxAcceptor<B> {
    service_name: String,
    phantom_data: PhantomData<B>,
}

impl<B: OwnedBuf> TransportAcceptor<IceoryxSinkAdapter<B>, IceoryxSourceAdapter>
    for IceoryxAcceptor<B>
{
    type Transport = IceoryxTransport;

    async fn accept(&self) -> anyhow::Result<Self::Transport> {
        debug!(service = %self.service_name, "IceoryxAcceptor: accepting (SERVER mode)");

        let node = NodeBuilder::new()
            .create::<Service>()
            .map_err(|e| anyhow::anyhow!("Iceoryx Node error: {:?}", e))?;

        let c2s_name = format!("{}_c2s", self.service_name);
        let s2c_name = format!("{}_s2c", self.service_name);

        let svc_c2s = port_service_factory(&node, &c2s_name)?;
        let subscriber = svc_c2s.subscriber_builder().create()?;

        let svc_evt_c2s = port_event_factory(&node, &c2s_name)?;
        let listener = svc_evt_c2s.listener_builder().create()?;

        let svc_s2c = port_service_factory(&node, &s2c_name)?;
        let publisher = svc_s2c
            .publisher_builder()
            .initial_max_slice_len(1024 * 1024)
            .max_loaned_samples(16)
            .create()?;

        let svc_evt_s2c = port_event_factory(&node, &s2c_name)?;
        let notifier = svc_evt_s2c.notifier_builder().create()?;

        Ok(IceoryxTransport {
            publisher,
            subscriber,
            notifier,
            listener,
            _node: node,
        })
    }
}

pub struct IceoryxBuilder<B> {
    base_name: String,
    phantom: PhantomData<B>,
}

impl<B: OwnedBuf> IceoryxBuilder<B> {
    pub fn new(service_name: &str) -> Self {
        Self {
            base_name: service_name.to_string(),
            phantom: PhantomData,
        }
    }
}

impl<B: OwnedBuf> TransportPerWorkerBuilder<IceoryxSinkAdapter<B>, IceoryxSourceAdapter>
    for IceoryxBuilder<B>
{
    type Transport = IceoryxTransport;
    type Acceptor = IceoryxAcceptor<B>;
    async fn bind(
        self,
        core_id: usize,
        registry: Option<&Arc<dyn TopologyRegistry>>,
    ) -> io::Result<Self::Acceptor> {
        let service_name = format!("{}_{}", self.base_name, core_id);

        info!(core_id, service_name = %service_name, "IceoryxBuilder: binding service to core");

        if let Some(reg) = registry {
            trace!(core_id, "Registering service in topology registry");
            reg.register(core_id, service_name.clone());
        }

        Ok(IceoryxAcceptor {
            service_name,
            phantom_data: PhantomData,
        })
    }
}

pub struct IceoryxSink(pub Publisher<Service, [u8], ()>);
pub struct IceoryxSource {
    pub sub: Subscriber<Service, [u8], ()>,
    pub id: u64,
}

pub struct IceoryxRawSink<Tx> {
    pub publisher: Publisher<Service, [u8], ()>,
    pub notifier: Notifier<Service>,
    pub phantom_data: PhantomData<Tx>,
}

impl<Tx: OwnedBuf> RawMessageSink for IceoryxRawSink<Tx> {
    type Message = Tx;

    fn poll_send(&self, _cx: &mut Context<'_>, data: &mut Option<Tx>) -> Poll<anyhow::Result<()>> {
        let msg = data.as_ref().expect("logic error: no data");
        let bytes = msg.as_ref();

        match self.publisher.loan_slice(bytes.len()) {
            Ok(mut sample_mut) => {
                sample_mut.payload_mut().copy_from_slice(bytes);

                if let Err(e) = sample_mut.send() {
                    error!(error = ?e, "Failed to send Iceoryx sample");
                    return Poll::Ready(Err(anyhow::anyhow!("{:?}", e)));
                }

                //TODO: notifications should be batched to avoid hammering the reactor —
                //      consider coalescing multiple sends into a single notify call
                match self.notifier.notify() {
                    Ok(0) => {
                        warn!(id = ?self.publisher.id(), "no one listen");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        error!(error = ?e, "Failed to send Iceoryx notification");
                        return Poll::Ready(Err(anyhow::anyhow!("{:?}", e)));
                    }
                }

                trace!(len = bytes.len(), "Iceoryx message sent successfully");
                Poll::Ready(Ok(()))
            }
            Err(LoanError::ExceedsMaxLoans) => {
                warn!("Iceoryx backpressure: ExceedsMaxLoans. Task will be waked.");
                _cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(e) => {
                error!(error = ?e, "Iceoryx loan error");
                Poll::Ready(Err(e.into()))
            }
        }
    }
}

pub struct IceoryxRawSource {
    pub subscriber: Subscriber<Service, [u8], ()>,
    pub attachment_id: WaitSetAttachmentId<Service>,
}

impl RawMessageSource for IceoryxRawSource {
    type Message = IceoryxPayload;

    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<Self::Message>> {
        for _ in 0..100_000 {
            if let Ok(Some(sample)) = self.subscriber.receive() {
                trace!(
                    len = sample.len(),
                    "Iceoryx message received (spin-loop path)"
                );
                return Poll::Ready(Ok(IceoryxPayload(sample)));
            }

            std::hint::spin_loop();
        }

        GlobalReactor::get().register(self.attachment_id.clone(), cx.waker().clone());

        if let Ok(Some(sample)) = self.subscriber.receive() {
            GlobalReactor::get().unregister(&self.attachment_id);
            trace!(
                len = sample.len(),
                "Iceoryx message received (after-registration path)"
            );
            return Poll::Ready(Ok(IceoryxPayload(sample)));
        }

        trace!("No data available after spin, falling asleep");
        Poll::Pending
    }
}

pub struct IceoryxSinkAdapter<Tx> {
    pub inner: Rc<IceoryxRawSink<Tx>>,
}

impl<Tx> Clone for IceoryxSinkAdapter<Tx> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<Tx: OwnedBuf> MessageSink for IceoryxSinkAdapter<Tx> {
    type Payload = Tx;

    async fn send(&self, data: Tx) -> anyhow::Result<()> {
        let mut slot = Some(data);
        poll_fn(|cx| self.inner.poll_send(cx, &mut slot)).await
    }
}

pub struct IceoryxSourceAdapter {
    pub inner: IceoryxRawSource,
}

impl MessageSource for IceoryxSourceAdapter {
    type Payload = IceoryxPayload;
    async fn recv(&mut self) -> Option<Self::Payload> {
        poll_fn(|cx| self.inner.poll_recv(cx)).await.ok()
    }
}

pub struct IceoryxTransport {
    pub publisher: Publisher<Service, [u8], ()>,
    pub subscriber: Subscriber<Service, [u8], ()>,
    pub notifier: Notifier<Service>,
    pub listener: Listener<Service>,
    pub _node: Node<Service>,
}

impl<Tx: OwnedBuf> Transport<IceoryxSinkAdapter<Tx>, IceoryxSourceAdapter> for IceoryxTransport {
    fn decompose(self) -> anyhow::Result<(IceoryxSinkAdapter<Tx>, IceoryxSourceAdapter)> {
        let reactor = GlobalReactor::get();
        let attachment_id = reactor.attach(self.listener);

        let raw_sink = IceoryxRawSink {
            publisher: self.publisher,
            notifier: self.notifier,
            phantom_data: PhantomData,
        };

        let raw_source = IceoryxRawSource {
            subscriber: self.subscriber,
            attachment_id,
        };

        Ok((
            IceoryxSinkAdapter {
                inner: Rc::new(raw_sink),
            },
            IceoryxSourceAdapter { inner: raw_source },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::serde_protocol::VecBuf;
    use compio::time::sleep;
    use futures::future::join;
    use std::future::ready;
    use std::time::{Duration, Instant};

    fn create_payload(data: &[u8]) -> VecBuf {
        Vec::from(data).into()
    }

    fn gen_service_name(suffix: &str) -> String {
        format!("ice_test_{}_{}", std::process::id(), suffix)
    }

    #[compio::test]
    async fn test_iceoryx_ping_pong_single_threaded() -> anyhow::Result<()> {
        // tracing_subscriber::fmt()
        //     .with_max_level(tracing::Level::TRACE)
        //     .init();

        let base_name = gen_service_name("pp");
        let service_full_name = format!("{}_0", base_name);

        let builder = IceoryxBuilder::<VecBuf>::new(&base_name);
        let acceptor = builder.bind(0, None).await?;

        let server_fut = async {
            let transport = acceptor.accept().await.expect("Accept failed");
            let (sink, mut source) = transport.decompose()?;

            if let Some(msg) = source.recv().await {
                assert_eq!(msg.as_ref(), b"ping");

                let response = create_payload(b"pong");
                sink.send(response).await?;
            } else {
                panic!("Server received nothing");
            }
            Ok::<_, anyhow::Error>(())
        };

        let client_fut = async {
            let connector = IceoryxConnector::<VecBuf>::new(&service_full_name);
            let transport = connector.connect().await?;
            let (sink, mut source) = transport.decompose()?;

            let instant = Instant::now();

            sink.send(create_payload(b"ping")).await?;

            if let Some(msg) = source.recv().await {
                let as_ref = msg.as_ref();
                assert_eq!(as_ref, b"pong");
            } else {
                panic!("Client received nothing");
            }

            let elapsed = instant.elapsed();
            println!("duration {:?}", elapsed);

            Ok::<_, anyhow::Error>(())
        };

        let (a, b) = join(server_fut, client_fut).await;
        a?;
        b?;
        Ok(())
    }

    // #[test]
    // fn bench_iceoryx() {
    //
    //
    //     crate::runtime::init(num_cpus::get());
    //
    //     let service_name = format!("ice_bench_{}", std::process::id());
    //
    //     let (done_tx, done_rx) = flume::bounded::<(Duration, usize)>(1);
    //
    //     let srv_service = service_name.clone();
    //
    //     let iterations = 100_000;
    //
    //
    //     crate::runtime::spawn_on(1, move || async move {
    //         let builder = IceoryxBuilder::new(&srv_service);
    //         let acceptor = builder.bind(1, None).await.unwrap();
    //
    //         let transport = acceptor.accept().await.expect("Accept failed");
    //         let (sink, mut source) = transport.decompose().unwrap();
    //
    //         for _ in 0..iterations {
    //             if let Some(msg) = source.recv().await {
    //                 sink.send(msg).await.unwrap();
    //             }
    //         }
    //
    //     });
    //
    //
    //     let clt_service = service_name.clone();
    //
    //     crate::runtime::spawn_on(2, move || async move {
    //         let full_name = format!("{}_1", clt_service);
    //
    //         sleep(Duration::from_millis(200)).await;
    //
    //         let connector = IceoryxConnector::new(&full_name);
    //         let transport = connector.connect().await.expect("Connect failed");
    //         let (sink, mut source) = transport.decompose().unwrap();
    //
    //
    //         let start = Instant::now();
    //
    //         for _ in 0..iterations {
    //             sink.send(create_payload(b"ping")).await.unwrap();
    //             let _res = source.recv().await.unwrap();
    //         }
    //
    //         let elapsed = start.elapsed();
    //
    //         done_tx.send((elapsed, iterations)).unwrap();
    //     });
    //
    //
    //     match done_rx.recv_timeout(Duration::from_secs(10)) {
    //         Ok((elapsed, iterations)) => {
    //             let avg_latency = elapsed / iterations as u32;
    //             let ops_per_sec = iterations as f64 / elapsed.as_secs_f64();
    //
    //             println!("========================================");
    //             println!("Benchmark Results:");
    //             println!("Total iterations: {}", iterations);
    //             println!("Total time: {:?}", elapsed);
    //             println!("Average Latency: {:?}", avg_latency);
    //             println!("Throughput: {:.2} ops/sec", ops_per_sec);
    //             println!("========================================");
    //         }
    //         Err(e) => {
    //             panic!("Test timed out or failed: {:?}", e);
    //         }
    //     }
    //
    // }
}
