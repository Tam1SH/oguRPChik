use anyhow::{Context, anyhow};
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

use crate::codecs::base::{
    BorrowedBuf, BufferAllocator, HasAllocator, MessageCodec, OwnedBuf, ReleasableBuf,
};
use crate::high::service_handler::ServiceHandler;
use crate::auth::handshake::{ConnectionGate, ConnectionMode, HandshakeMode};
use crate::low::runtime;
use crate::low::server_worker::ServerWorker;
use crate::transport::base::pool_config::PoolConfig;
use crate::transport::base::{
    MessageSink, MessageSource, TopologyRegistry, TransportBuilder, TransportPerWorkerBuilder,
};

pub struct NoHandler;
pub struct NoCodec;
pub struct NoTransport;
pub struct NoSink;
pub struct NoSource;

pub struct ServerBuilder<H, C, T, Si, So, RxPayload> {
    cores: usize,
    handler: H,
    transport: Option<T>,
    registry: Option<Arc<dyn TopologyRegistry>>,
    handshake: HandshakeMode,
    connection_mode: ConnectionMode,
    _phantom: PhantomData<(C, Si, So, RxPayload)>,
}

pub fn setup() -> ServerBuilder<NoHandler, NoCodec, NoTransport, NoSink, NoSource, ()> {
    ServerBuilder {
        cores: 1,
        handler: NoHandler,
        transport: None,
        registry: None,
        handshake: HandshakeMode::Disabled,
        connection_mode: ConnectionMode::OneToMany,
        _phantom: PhantomData,
    }
}

impl<H, C, T, Si, So, RxPayload> ServerBuilder<H, C, T, Si, So, RxPayload> {
    pub fn single_thread(mut self) -> Self {
        self.cores = 1;
        self
    }

    pub fn threads(mut self, n: usize) -> Self {
        self.cores = n;
        self
    }

    pub fn with_registry(mut self, registry: Arc<dyn TopologyRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn with_handshake(mut self, handshake: HandshakeMode) -> Self {
        self.handshake = handshake;
        self
    }

    pub fn one_to_one(mut self) -> Self {
        self.connection_mode = ConnectionMode::OneToOne;
        self
    }

    pub fn one_to_many(mut self) -> Self {
        self.connection_mode = ConnectionMode::OneToMany;
        self
    }

    pub fn service<NewH, NewC>(
        self,
        handler: NewH,
    ) -> ServerBuilder<NewH, NewC, T, Si, So, RxPayload>
    where
        NewH: ServiceHandler<NewC>,
        NewC: MessageCodec,
        NewC::Dest: HasAllocator,
    {
        ServerBuilder {
            cores: self.cores,
            handler,
            registry: self.registry,
            transport: self.transport,
            handshake: self.handshake,
            connection_mode: self.connection_mode,
            _phantom: PhantomData,
        }
    }

    pub fn with_transport<NewT, NewSi, NewSo, P, NewPayload>(
        self,
        transport: NewT,
    ) -> ServerBuilder<H, C, NewT, NewSi, NewSo, NewPayload>
    where
        NewSi: MessageSink,
        NewSo: MessageSource,
        NewPayload: BorrowedBuf,
        NewT: TransportBuilder<P, Rx = NewPayload>,
        P: OwnedBuf,
    {
        ServerBuilder {
            cores: self.cores,
            handler: self.handler,
            registry: self.registry,
            transport: Some(transport),
            handshake: self.handshake,
            connection_mode: self.connection_mode,
            _phantom: PhantomData,
        }
    }
}

impl<H, C, T, Si, So, RxPayload> ServerBuilder<H, C, T, Si, So, RxPayload>
where
    C: MessageCodec,
    C::Dest: HasAllocator,
    <C::Dest as HasAllocator>::Alloc: BufferAllocator<Payload = C::Dest>,
    H: ServiceHandler<C>,
    Si: MessageSink<Payload = C::Dest>,
    So: MessageSource<Payload = RxPayload>,
    T: TransportBuilder<C::Dest, Rx = RxPayload>,
    RxPayload: ReleasableBuf,
    <T as TransportBuilder<C::Dest>>::Builder: TransportPerWorkerBuilder<Si, So>,
{
    pub async fn run(self) -> anyhow::Result<std::convert::Infallible> {
        let transport = self.transport.ok_or_else(|| anyhow!("Transport not set"))?;
        let registry = self.registry.ok_or_else(|| anyhow!("Registry not set"))?;

        if registry.transport_name() != transport.kind() {
            return Err(anyhow!("Registry transport mismatch"));
        }

        if registry.codec_name() != C::kind() {
            return Err(anyhow!("Registry codec mismatch"));
        }

        runtime::init(self.cores);
        registry.init_cores(self.cores);

        info!(
            cores = self.cores,
            payload = std::any::type_name::<C::Dest>(),
            "Starting RPC server workers"
        );

        type Alloc<C> = <<C as MessageCodec>::Dest as HasAllocator>::Alloc;
        let connection_gate = ConnectionGate::new(self.connection_mode);

        for core_id in 0..self.cores {
            let h = self.handler.clone();
            let builder = transport.server_builder(core_id);
            let allocator = Alloc::<C>::get(&PoolConfig::default());

            ServerWorker::<(C, Alloc<C>, RxPayload), H>::spawn(
                core_id,
                builder,
                h,
                Some(registry.clone()),
                allocator,
                connection_gate.clone(),
                self.handshake.clone(),
            )
            .await
            .with_context(|| format!("Failed to spawn worker on core {}", core_id))?;
        }

        loop {
            compio::time::sleep(Duration::from_secs(3600)).await;
        }
    }
}
