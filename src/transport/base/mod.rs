use std::io;
use std::sync::Arc;
use std::task::{Context, Poll};

pub trait WorkerInitializer {
    fn init(core_id: usize);
}

pub struct NoOpInitializer;
impl WorkerInitializer for NoOpInitializer {
    fn init(_core_id: usize) {}
}

pub trait RawMessageSink: 'static {
    type Message;
    fn poll_send(
        &self,
        cx: &mut Context<'_>,
        data: &mut Option<Self::Message>
    ) -> Poll<anyhow::Result<()>>;
}

pub trait RawMessageSource: 'static {
    type Message;

    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<Self::Message>>;
}

pub trait MessageSink: Clone + 'static {
    
    type Payload: 'static;

    async fn send(&self, data: Self::Payload) -> anyhow::Result<()>;
}

pub trait MessageSource: 'static {
    type Payload: AsRef<[u8]> + 'static;
    async fn recv(&mut self) -> Option<Self::Payload>;
}

pub trait Transport<Sink: MessageSink, Source: MessageSource>: 'static {
    fn decompose(self) -> anyhow::Result<(Sink, Source)>;
}



pub trait TransportConnector<Sink: MessageSink, Source: MessageSource>: Send + 'static {
    type Transport: Transport<Sink, Source>;

    async fn connect(&self) -> anyhow::Result<Self::Transport>;
}

pub trait TransportAcceptor<Sink: MessageSink, Source: MessageSource>: 'static {
    type Transport: Transport<Sink, Source>;

    async fn accept(&self) -> anyhow::Result<Self::Transport>;

}


#[derive(Clone)]
pub struct Endpoint {
    pub kind: String,
    pub addr: String,
}

pub trait TopologyRegistry: Send + Sync + 'static {
    fn init_cores(&self, cores: usize);
    fn register(&self, core_id: usize, endpoint: String);
    fn transport_name(&self) -> &str;
    fn codec_name(&self) -> &str;
}

pub trait TransportBuilder<P: AsRef<[u8]>> {
    type Si: MessageSink<Payload = P> + 'static;
    type So: MessageSource<Payload = P> + 'static;
    type Builder: TransportPerWorkerBuilder<Self::Si, Self::So>;
    type Connector: TransportConnector<Self::Si, Self::So>;

    fn kind(&self) -> String;

    fn server_builder(&self) -> Self::Builder;

    fn client_connector(&self, endpoint: String) -> anyhow::Result<Self::Connector>;
}


pub trait TransportPerWorkerBuilder<Sink: MessageSink, Source: MessageSource>: Send + Sync + 'static {
    type Transport: Transport<Sink, Source>;
    type Acceptor: TransportAcceptor<Sink, Source, Transport = Self::Transport>;
    type Initializer: WorkerInitializer;

    async fn bind(
        self,
        core_id: usize,
        registry: Option<&Arc<dyn TopologyRegistry>>
    ) -> io::Result<Self::Acceptor>;
}

pub trait BufferAllocator: Clone + 'static {
    type Payload: AsRef<[u8]> + 'static;
    fn allocate(size_hint: usize) -> Self::Payload;
}
