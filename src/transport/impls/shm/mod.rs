mod reactor;

use std::cell::RefCell;
use std::future::poll_fn;
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use anyhow::Error;
use compio::buf::SetLen;
use iceoryx2::node::{Node, NodeBuilder};
use iceoryx2::port::listener::Listener;
use iceoryx2::port::LoanError;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::{ipc, Service, WaitSet, WaitSetAttachmentId, WaitSetGuard};
use iceoryx2::sample::Sample;
use iceoryx2::service::port_factory::publish_subscribe::PortFactory;
use iceoryx2::waitset::WaitSetBuilder;
use tracing::{debug, error, info, trace, warn};
use crate::align_buffer::AlignedBuffer;
use crate::server::HasDefaultAllocator;
use crate::rkyv_protocol::RkyvAllocator;
use crate::tpc_pool::TpcPool;
use crate::transport::base::{BufferAllocator, MessageSink, MessageSource, RawMessageSink, RawMessageSource, TopologyRegistry, Transport, TransportAcceptor, TransportPerWorkerBuilder, TransportConnector, NoOpInitializer};
use crate::transport::impls::shm::reactor::GlobalReactor;

fn port_service_factory(node: &Node<ipc::Service>, name: &str) -> Result<PortFactory<ipc::Service, [u8], ()>, Error> {
    node.service_builder(&name.try_into()?)
        .publish_subscribe::<[u8]>()
        .open_or_create()
        .map_err(|e| anyhow::anyhow!("Failed to open service {}: {:?}", name, e))
}

fn port_event_factory(node: &Node<ipc::Service>, base_name: &str) -> Result<iceoryx2::service::port_factory::event::PortFactory<ipc::Service>, Error> {
    let ev_name = format!("{}_events", base_name);
    node.service_builder(&ev_name.as_str().try_into()?)
        .event()
        .open_or_create()
        .map_err(|e| anyhow::anyhow!("Failed to open event service {}: {:?}", ev_name, e))
}

pub struct IceoryxConnector {
    service_name: String,
}

impl IceoryxConnector {
    pub fn new(service_name: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
        }
    }
}

impl TransportConnector<IceoryxSinkAdapter, IceoryxSourceAdapter> for IceoryxConnector {
    type Transport = IceoryxTransport;

    async fn connect(&self) -> anyhow::Result<Self::Transport> {
        debug!(service = %self.service_name, "IceoryxConnector: connecting to service");

        let node = NodeBuilder::new()
            .create::<ipc::Service>()
            .map_err(|e| anyhow::anyhow!("Iceoryx Node error: {:?}", e))?;

        let c2s_name = format!("{}_c2s", self.service_name);
        let s2c_name = format!("{}_s2c", self.service_name);

        let svc_c2s = port_service_factory(&node, &c2s_name)?;
        let publisher = svc_c2s.publisher_builder()
            .initial_max_slice_len(1024 * 1024)
            .max_loaned_samples(4)
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


pub struct IceoryxAcceptor {
    service_name: String,
}

impl TransportAcceptor<IceoryxSinkAdapter, IceoryxSourceAdapter> for IceoryxAcceptor {
    type Transport = IceoryxTransport;

    async fn accept(&self) -> anyhow::Result<Self::Transport> {
        debug!(service = %self.service_name, "IceoryxAcceptor: accepting (SERVER mode)");

        let node = NodeBuilder::new()
            .create::<ipc::Service>()
            .map_err(|e| anyhow::anyhow!("Iceoryx Node error: {:?}", e))?;

        let c2s_name = format!("{}_c2s", self.service_name);
        let s2c_name = format!("{}_s2c", self.service_name);

        let svc_c2s = port_service_factory(&node, &c2s_name)?;
        let subscriber = svc_c2s.subscriber_builder().create()?;

        let svc_evt_c2s = port_event_factory(&node, &c2s_name)?;
        let listener = svc_evt_c2s.listener_builder().create()?;

        let svc_s2c = port_service_factory(&node, &s2c_name)?;
        let publisher = svc_s2c.publisher_builder()
            .initial_max_slice_len(1024 * 1024)
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

pub struct IceoryxBuilder {
    base_name: String,
}

impl IceoryxBuilder {
    pub fn new(service_name: &str) -> Self {
        Self {
            base_name: service_name.to_string(),
        }
    }
}

impl TransportPerWorkerBuilder<IceoryxSinkAdapter, IceoryxSourceAdapter> for IceoryxBuilder {
    type Transport = IceoryxTransport;
    type Acceptor = IceoryxAcceptor;
    type Initializer = NoOpInitializer;
    async fn bind(
        self,
        core_id: usize,
        registry: Option<&Arc<dyn TopologyRegistry>>
    ) -> io::Result<Self::Acceptor>
    {
        let service_name = format!("{}_{}", self.base_name, core_id);

        info!(core_id, service_name = %service_name, "IceoryxBuilder: binding service to core");

        if let Some(reg) = registry {
            trace!(core_id, "Registering service in topology registry");
            reg.register(core_id, service_name.clone());
        }

        Ok(IceoryxAcceptor {
            service_name,
        })
    }
}


pub struct IceoryxSink<S: Service>(pub Publisher<S, [u8], ()>);
pub struct IceoryxSource<S: Service> {
    pub sub: Subscriber<S, [u8], ()>,
    pub id: u64,
}


pub struct IceoryxRawSink {
    pub publisher: Publisher<ipc::Service, [u8], ()>,
    pub notifier: Notifier<ipc::Service>,
}

impl RawMessageSink for IceoryxRawSink {
    type Message = AlignedBuffer;

    fn poll_send(&self, _cx: &mut Context<'_>, data: &mut Option<AlignedBuffer>) -> Poll<anyhow::Result<()>> {
        let msg = data.as_ref().expect("logic error: no data");
        let bytes = msg.0.as_slice();

        match self.publisher.loan_slice(bytes.len()) {
            Ok(mut sample_mut) => {

                sample_mut.payload_mut().copy_from_slice(bytes);

                if let Err(e) = sample_mut.send() {
                    error!(error = ?e, "Failed to send Iceoryx sample");
                    return Poll::Ready(Err(anyhow::anyhow!("{:?}", e)));
                }

                match self.notifier.notify() {
                    Ok(0) => {
                        warn!("no one listen");
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
    pub subscriber: Subscriber<ipc::Service, [u8], ()>,
    pub attachment_id: WaitSetAttachmentId<ipc::Service>,
}


impl RawMessageSource for IceoryxRawSource {
    type Message = AlignedBuffer;

    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<AlignedBuffer>> {

        if let Ok(Some(sample)) = self.subscriber.receive() {

            trace!(len = sample.len(), "Iceoryx message received (fast path)");
            return Poll::Ready(Ok(self.extract_buffer(sample)));
        }

        trace!("No data available, registering waker");
        GlobalReactor::get().register(self.attachment_id.clone(), cx.waker().clone());

        if let Ok(Some(sample)) = self.subscriber.receive() {
            trace!(len = sample.len(), "Iceoryx message received (after registration)");
            return Poll::Ready(Ok(self.extract_buffer(sample)));
        }

        Poll::Pending
    }
}

impl IceoryxRawSource {
    fn extract_buffer(&self, sample: Sample<ipc::Service, [u8], ()>) -> AlignedBuffer {
        let mut aligned_buf = TpcPool::acquire_body(sample.len());
        unsafe {
            std::ptr::copy_nonoverlapping(
                sample.as_ptr(),
                aligned_buf.as_mut_ptr(),
                sample.len()
            );
            aligned_buf.set_len(sample.len());
        }
        aligned_buf
    }
}


pub struct IceoryxSinkAdapter {
    pub inner: Rc<IceoryxRawSink>,
}

impl Clone for IceoryxSinkAdapter {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}


impl MessageSink for IceoryxSinkAdapter {
    type Payload = AlignedBuffer;

    async fn send(&self, data: AlignedBuffer) -> anyhow::Result<()> {
        let mut slot = Some(data);
        poll_fn(|cx| self.inner.poll_send(cx, &mut slot)).await
    }
}

pub struct IceoryxSourceAdapter {
    pub inner: IceoryxRawSource,
}

impl MessageSource for IceoryxSourceAdapter {
    type Payload = AlignedBuffer;
    async fn recv(&mut self) -> Option<AlignedBuffer> {
        poll_fn(|cx| self.inner.poll_recv(cx)).await.ok()
    }
}

pub struct IceoryxTransport {
    pub publisher: Publisher<ipc::Service, [u8], ()>,
    pub subscriber: Subscriber<ipc::Service, [u8], ()>,
    pub notifier: Notifier<ipc::Service>,
    pub listener: Listener<ipc::Service>,
    pub _node: Node<ipc::Service>,
}

impl Transport<IceoryxSinkAdapter, IceoryxSourceAdapter> for IceoryxTransport
{
    fn decompose(self) -> anyhow::Result<(IceoryxSinkAdapter, IceoryxSourceAdapter)> {

        let reactor = GlobalReactor::get();
        let attachment_id = reactor.attach(self.listener);

        let raw_sink = IceoryxRawSink {
            publisher: self.publisher,
            notifier: self.notifier
        };

        let raw_source = IceoryxRawSource {
            subscriber: self.subscriber,
            attachment_id,
        };

        Ok((
            IceoryxSinkAdapter { inner: Rc::new(raw_sink) },
            IceoryxSourceAdapter { inner: raw_source }
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::future::ready;
    use super::*;
    use compio::time::sleep;
    use std::time::{Duration, Instant};
    use futures::future::join;

    use crate::utils::current_cpu_core;


    fn create_payload(data: &[u8]) -> AlignedBuffer {
        let mut buf = TpcPool::acquire_body(data.len());
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf.as_mut_ptr(), data.len());
            buf.set_len(data.len());
        }
        buf
    }

    fn gen_service_name(suffix: &str) -> String {
        format!("ice_test_{}_{}", std::process::id(), suffix)
    }

    #[compio::test]
    async fn test_iceoryx_ping_pong_single_threaded() -> anyhow::Result<()> {

        tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).init();


        let base_name = gen_service_name("pp");
        let service_full_name = format!("{}_0", base_name);

        let builder = IceoryxBuilder::new(&base_name);
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

            let connector = IceoryxConnector::new(&service_full_name);
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

    #[test]
    fn bench_iceoryx() {

        tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG).init();

        crate::runtime::init(num_cpus::get());

        let service_name = format!("ice_bench_{}", std::process::id());

        let (done_tx, done_rx) = flume::bounded::<(Duration, usize)>(1);

        let srv_service = service_name.clone();

        let iterations = 100_000;


        crate::runtime::spawn_on(1, move || async move {
            let builder = IceoryxBuilder::new(&srv_service);
            let acceptor = builder.bind(1, None).await.unwrap();

            let transport = acceptor.accept().await.expect("Accept failed");
            let (sink, mut source) = transport.decompose().unwrap();

            for _ in 0..iterations {
                if let Some(msg) = source.recv().await {
                    sink.send(msg).await.unwrap();
                }
            }

        });


        let clt_service = service_name.clone();

        crate::runtime::spawn_on(2, move || async move {
            let full_name = format!("{}_1", clt_service);

            sleep(Duration::from_millis(200)).await;

            let connector = IceoryxConnector::new(&full_name);
            let transport = connector.connect().await.expect("Connect failed");
            let (sink, mut source) = transport.decompose().unwrap();


            let start = Instant::now();

            for _ in 0..iterations {
                sink.send(create_payload(b"ping")).await.unwrap();
                let _res = source.recv().await.unwrap();
            }

            let elapsed = start.elapsed();

            done_tx.send((elapsed, iterations)).unwrap();
        });


        match done_rx.recv_timeout(std::time::Duration::from_secs(1000)) {
            Ok((elapsed, iterations)) => {
                let avg_latency = elapsed / iterations as u32;
                let ops_per_sec = iterations as f64 / elapsed.as_secs_f64();

                info!("========================================");
                info!("Benchmark Results:");
                info!("Total iterations: {}", iterations);
                info!("Total time: {:?}", elapsed);
                info!("Average Latency: {:?}", avg_latency);
                info!("Throughput: {:.2} ops/sec", ops_per_sec);
                info!("========================================");
            }
            Err(e) => {
                panic!("Test timed out or failed: {:?}", e);
            }
        }

    }
}