pub mod pool_config;

use crate::codecs::base::{BorrowedBuf, OwnedBuf};
use std::io;
use std::sync::Arc;
use std::task::{Context, Poll};

pub trait RawMessageSink: 'static {
    type Message;
    fn poll_send(
        &self,
        cx: &mut Context<'_>,
        data: &mut Option<Self::Message>,
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
    type Payload: BorrowedBuf;
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

pub trait TransportBuilder<Tx: OwnedBuf>: Clone + 'static {
    type Rx: BorrowedBuf;
    type Si: MessageSink<Payload = Tx> + 'static;
    type So: MessageSource<Payload = Self::Rx> + 'static;
    type Builder: TransportPerWorkerBuilder<Self::Si, Self::So>;
    type Connector: TransportConnector<Self::Si, Self::So>;

    fn kind(&self) -> String;

    fn server_builder(&self, core_id: usize) -> Self::Builder;

    fn client_connector(&self, endpoint: String, core_id: usize)
    -> anyhow::Result<Self::Connector>;
}

pub trait TransportPerWorkerBuilder<Sink: MessageSink, Source: MessageSource>:
    Send + 'static
{
    type Transport: Transport<Sink, Source>;
    type Acceptor: TransportAcceptor<Sink, Source, Transport = Self::Transport>;

    async fn bind(
        self,
        core_id: usize,
        registry: Option<&Arc<dyn TopologyRegistry>>,
    ) -> io::Result<Self::Acceptor>;
}
