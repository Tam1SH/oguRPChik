use crate::codecs::base::{BufferAllocator, OwnedBuf};
use crate::transport::base::{
    TopologyRegistry, Transport, TransportAcceptor, TransportConnector, TransportPerWorkerBuilder,
};
use crate::transport::impls::peer::config::PeerConfig;
use crate::transport::impls::peer::handle::{PeerSink, PeerSource};
use crate::transport::impls::peer::implementation::Peer;
use crate::transport::stream::{Acceptor, AcceptorBuilder, AsyncStream, Connector, StreamBuf};
use std::io;
use std::marker::PhantomData;
use std::sync::Arc;

pub struct StreamTransport<S: AsyncStream, B: StreamBuf, A: BufferAllocator<Payload = B>> {
    stream: S,
    config: PeerConfig,
    allocator: A,
}

impl<S: AsyncStream, B: StreamBuf, A: BufferAllocator<Payload = B>> StreamTransport<S, B, A> {
    pub fn new(stream: S, config: PeerConfig, allocator: A) -> Self {
        Self { stream, config, allocator }
    }
}

impl<S: AsyncStream, B: StreamBuf, A: BufferAllocator<Payload = B>>
Transport<PeerSink<B>, PeerSource<B>> for StreamTransport<S, B, A>
{
    fn decompose(self) -> anyhow::Result<(PeerSink<B>, PeerSource<B>)> {
        Peer::new(self.stream, self.config, self.allocator).map_err(Into::into)
    }
}

// ── Acceptor ──────────────────────────────────────────────────────────────────

pub struct GenericStreamAcceptor<A: Acceptor, B: StreamBuf, Alloc: BufferAllocator<Payload = B>> {
    inner: A,
    config: PeerConfig,
    allocator: Alloc,
}

impl<A: Acceptor, B: StreamBuf, Alloc: BufferAllocator<Payload = B>>
TransportAcceptor<PeerSink<B>, PeerSource<B>> for GenericStreamAcceptor<A, B, Alloc>
{
    type Transport = StreamTransport<A::Stream, B, Alloc>;

    async fn accept(&self) -> anyhow::Result<Self::Transport> {
        let (stream, _) = self.inner.accept().await?;
        Ok(StreamTransport::new(stream, self.config.clone(), self.allocator.clone()))
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

pub struct GenericStreamBuilder<AB: AcceptorBuilder, B: StreamBuf, Alloc: BufferAllocator<Payload = B>> {
    inner_builder: AB,
    config: PeerConfig,
    allocator: Alloc,
    _marker: PhantomData<B>,
}

impl<AB: AcceptorBuilder, B: StreamBuf, Alloc: BufferAllocator<Payload = B>>
GenericStreamBuilder<AB, B, Alloc>
{
    pub fn new(inner_builder: AB, config: PeerConfig, allocator: Alloc) -> Self {
        Self { inner_builder, config, allocator, _marker: PhantomData }
    }
}

impl<AB: AcceptorBuilder, B: StreamBuf, Alloc: BufferAllocator<Payload = B> + Send>
TransportPerWorkerBuilder<PeerSink<B>, PeerSource<B>> for GenericStreamBuilder<AB, B, Alloc>
{
    type Transport = StreamTransport<AB::Stream, B, Alloc>;
    type Acceptor = GenericStreamAcceptor<AB::Acceptor, B, Alloc>;

    async fn bind(
        self,
        core_id: usize,
        registry: Option<&Arc<dyn TopologyRegistry>>,
    ) -> io::Result<Self::Acceptor> {
        let actual_addr = self.inner_builder.local_addr()?.to_string();
        let acceptor = self.inner_builder.bind().await?;
        if let Some(reg) = registry {
            reg.register(core_id, actual_addr);
        }
        Ok(GenericStreamAcceptor {
            inner: acceptor,
            config: self.config,
            allocator: self.allocator,
        })
    }
}

// ── Connector ─────────────────────────────────────────────────────────────────

pub struct GenericStreamConnector<C: Connector, B: StreamBuf, Alloc: BufferAllocator<Payload = B>> {
    inner: C,
    config: PeerConfig,
    allocator: Alloc,
    _marker: PhantomData<B>,
}

impl<C: Connector, B: StreamBuf, Alloc: BufferAllocator<Payload = B>>
GenericStreamConnector<C, B, Alloc>
{
    pub fn new(inner: C, config: PeerConfig, allocator: Alloc) -> Self {
        Self { inner, config, allocator, _marker: PhantomData }
    }
}

impl<C: Connector, B: StreamBuf, Alloc: BufferAllocator<Payload = B> + Send>
TransportConnector<PeerSink<B>, PeerSource<B>> for GenericStreamConnector<C, B, Alloc>
{
    type Transport = StreamTransport<C::Stream, B, Alloc>;

    async fn connect(&self) -> anyhow::Result<Self::Transport> {
        let stream = self.inner.connect().await?;
        Ok(StreamTransport::new(stream, self.config.clone(), self.allocator.clone()))
    }
}