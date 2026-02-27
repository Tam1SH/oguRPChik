use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;
use anyhow::{anyhow, Context};
use tracing::info;

use crate::message_codec::MessageCodec;
use crate::worker::ServerWorker;
use crate::runtime;
use crate::service_handler::ServiceHandler;
use crate::transport::base::{BufferAllocator, MessageSink, MessageSource, TransportBuilder, TopologyRegistry, TransportPerWorkerBuilder, WorkerInitializer};
use crate::transport::discovery::Topology;

pub struct NoHandler;
pub struct NoCodec;
pub struct NoTransport;
pub struct NoSink;
pub struct NoSource;

pub trait HasDefaultAllocator: AsRef<[u8]> + 'static {
    type Alloc: BufferAllocator<Payload = Self> + Send + Sync + 'static;
}

impl HasDefaultAllocator for Vec<u8> {
    type Alloc = DefaultVecAlloc;
}


#[derive(Clone)]
pub struct DefaultVecAlloc;
impl BufferAllocator for DefaultVecAlloc {
    type Payload = Vec<u8>;
    fn allocate(size: usize) -> Vec<u8> { Vec::with_capacity(size) }
}



pub struct ServerBuilder<H, C, T, Si, So> {
    cores: usize,
    handler: H,
    transport: Option<T>,
    registry: Option<Arc<dyn TopologyRegistry>>,
    topology: Option<Topology>,
    peer_tx: Option<flume::Sender<Option<Topology>>>,
    _phantom: PhantomData<(C, Si, So)>,
}

pub fn setup() -> ServerBuilder<NoHandler, NoCodec, NoTransport, NoSink, NoSource> {
    ServerBuilder {
        cores: num_cpus::get(),
        handler: NoHandler,
        transport: None,
        registry: None,
        peer_tx: None,
        _phantom: PhantomData,
        topology: None,
    }
}

impl<H, C, T, Si, So> ServerBuilder<H, C, T, Si, So> {

    pub fn single_thread(mut self) -> Self {
        self.cores = 1;
        self
    }
    pub fn on_peer(self, tx: flume::Sender<Option<Topology>>) -> Self {
        Self { peer_tx: Some(tx), ..self }
    }
    pub fn cores(mut self, cores: usize) -> Self {
        self.cores = cores;
        self
    }

    pub fn service<NewH, NewC>(self, handler: NewH) -> ServerBuilder<NewH, NewC, T, Si, So>
    where
        NewH: ServiceHandler<NewC>,
        NewC: MessageCodec,
        NewC::Dest: HasDefaultAllocator,
    {
        ServerBuilder {
            cores: self.cores,
            handler,
            peer_tx: self.peer_tx,
            registry: self.registry,
            transport: self.transport,
            _phantom: PhantomData,
            topology: self.topology,
        }
    }

    pub fn with_registry(self, registry: Arc<dyn TopologyRegistry>) -> Self {
        Self {
            cores: self.cores,
            handler: self.handler,
            registry: Some(registry),
            transport: self.transport,
            peer_tx: self.peer_tx,
            _phantom: PhantomData,
            topology: self.topology,
        }
    }

    pub fn announce(self, topology: Topology) -> Self {
        Self { topology: Some(topology), ..self }
    }

    pub fn with_transport<NewT, NewSi, NewSo, P>(
        self,
        transport: NewT,
    ) -> ServerBuilder<H, C, NewT, NewSi, NewSo>
    where
        NewSi: MessageSink,
        NewSo: MessageSource,
        NewT: TransportBuilder<P>,
        P: AsRef<[u8]>
    {
        ServerBuilder {
            cores: self.cores,
            handler: self.handler,
            registry: self.registry,
            peer_tx: self.peer_tx,
            transport: Some(transport),
            _phantom: PhantomData,
            topology: self.topology,
        }
    }
}


impl<H, C, T, Si, So> ServerBuilder<H, C, T, Si, So>
where
    C: MessageCodec + 'static,
    C::Dest: HasDefaultAllocator,
    H: ServiceHandler<C> + Clone + Send + Sync + 'static,
    Si: MessageSink<Payload = C::Dest>,
    So: MessageSource<Payload = C::Dest>,
    T: TransportBuilder<C::Dest> + Send + Sync + 'static,
    <T as TransportBuilder<C::Dest>>::Builder: TransportPerWorkerBuilder<Si, So>
{
    pub async fn run(self) -> anyhow::Result<std::convert::Infallible>
    {
        let transport = self.transport.ok_or_else(|| anyhow!("Transport not set"))?;
        let registry = self.registry.ok_or_else(|| anyhow!("Registry not set"))?;

        if registry.transport_name() != transport.kind() {
            return Err(anyhow!("Registry transport mismatch"));
        }

        let codec_name = C::kind();
        if registry.codec_name() != codec_name {
            return Err(anyhow!("Registry codec mismatch"));
        }

        runtime::init(self.cores);
        registry.init_cores(self.cores);

        info!(
            cores = self.cores,
            payload = std::any::type_name::<C::Dest>(),
            "Starting RPC server workers"
        );

        for core_id in 0..self.cores {
            let h = self.handler.clone();

            let builder = transport.server_builder();

            <T::Builder as TransportPerWorkerBuilder<Si, So>>::Initializer::init(core_id);

            type SelectedAlloc<C> = <<C as MessageCodec>::Dest as HasDefaultAllocator>::Alloc;

            ServerWorker::<(C, SelectedAlloc<C>), H>::spawn(
                core_id,
                builder,
                h,
                Some(registry.clone()),
                self.peer_tx.clone(),
                self.topology.clone()
            )
                .with_context(|| format!("Failed to spawn worker on core {}", core_id))?;
        }

        loop {
            compio::time::sleep(Duration::from_secs(3600)).await;
        }
    }
}