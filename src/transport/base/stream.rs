use crate::align_buffer::AlignedBuffer;

use crate::transport::stream::{Acceptor, AcceptorBuilder, AsyncStream, Connector};
use std::io;
use std::sync::Arc;
use local_sync::mpsc;
use crate::transport::base::handle::{PeerSink, PeerSource};
use crate::transport::base::{MessageSink, MessageSource, NoOpInitializer, TopologyRegistry, Transport, TransportAcceptor, TransportPerWorkerBuilder, TransportConnector};
use crate::transport::impls::peer::{Peer, PeerConfig};

pub struct StreamTransport<S: AsyncStream> {
    pub stream: S,
    pub config: PeerConfig,
}

impl<S: AsyncStream> Transport<PeerSink, PeerSource> for StreamTransport<S> {
    fn decompose(self) -> anyhow::Result<(PeerSink, PeerSource)> {
        Peer::new(self.stream, self.config).map_err(Into::into)
    }
}

pub struct GenericStreamAcceptor<A: Acceptor> {
    pub(super) inner: A,
    config: PeerConfig,
}

impl<A: Acceptor> TransportAcceptor<PeerSink, PeerSource> for GenericStreamAcceptor<A> {
    type Transport = StreamTransport<A::Stream>;

    async fn accept(&self) -> anyhow::Result<Self::Transport> {
        let (stream, _) = self.inner.accept().await?;
        let transport = StreamTransport {
            stream,
            config: self.config.clone(),
        };
        Ok(transport)
    }

}

pub struct GenericStreamBuilder<B: AcceptorBuilder> {
    inner_builder: B,
    config: PeerConfig,
}

impl<B: AcceptorBuilder> GenericStreamBuilder<B> {
    pub fn new(inner_builder: B, config: PeerConfig) -> Self {
        Self {
            inner_builder,
            config,
        }
    }
}

impl<B: AcceptorBuilder> TransportPerWorkerBuilder<PeerSink, PeerSource> for GenericStreamBuilder<B> {
    type Transport = StreamTransport<B::Stream>;
    type Acceptor = GenericStreamAcceptor<B::Acceptor>;
    type Initializer = NoOpInitializer;

    async fn bind(
        self,
        core_id: usize,
        registry: Option<&Arc<dyn TopologyRegistry>>
    ) -> io::Result<Self::Acceptor>  {

        let actual_addr = self.inner_builder.local_addr()?.to_string();

        let acceptor = self.inner_builder.bind().await?;

        if let Some(reg) = registry {
            reg.register(core_id, actual_addr);
        }

        Ok(GenericStreamAcceptor {
            inner: acceptor,
            config: self.config,
        })
    }
}

pub struct GenericStreamConnector<C: Connector> {
    inner: C,
    config: PeerConfig,
}

impl<C: Connector> GenericStreamConnector<C> {
    pub fn new(inner: C, config: PeerConfig) -> Self {
        Self { inner, config }
    }
}

impl<C: Connector> TransportConnector<PeerSink, PeerSource> for GenericStreamConnector<C> {
    type Transport = StreamTransport<C::Stream>;

    async fn connect(&self) -> anyhow::Result<Self::Transport> {
        let stream = self.inner.connect().await?;
        Ok(StreamTransport {
            stream,
            config: self.config.clone(),
        })
    }
}
