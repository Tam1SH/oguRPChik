use crate::codecs::base::BufferAllocator;
use crate::pool::buf_guard::BufGuard;
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

pub struct StreamTransport<S, B, TxA, RxA>
where
    S: AsyncStream,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B>,
    RxA: BufferAllocator<Payload = B, SendMark = ()>,
{
    stream: S,
    config: PeerConfig,
    tx_allocator: TxA,
    rx_allocator: RxA,
    _marker: PhantomData<B>,
}

impl<S, B, TxA, RxA> StreamTransport<S, B, TxA, RxA>
where
    S: AsyncStream,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B>,
    RxA: BufferAllocator<Payload = B, SendMark = ()>,
{
    pub fn new(stream: S, config: PeerConfig, tx_allocator: TxA, rx_allocator: RxA) -> Self {
        Self {
            stream,
            config,
            tx_allocator,
            rx_allocator,
            _marker: PhantomData,
        }
    }
}

impl<S, B, TxA, RxA> Transport<PeerSink<B>, PeerSource<BufGuard<B, RxA>>>
    for StreamTransport<S, B, TxA, RxA>
where
    S: AsyncStream + Clone + 'static,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B>,
    RxA: BufferAllocator<Payload = B, SendMark = ()>,
{
    fn decompose(self) -> anyhow::Result<(PeerSink<B>, PeerSource<BufGuard<B, RxA>>)> {
        Peer::new(
            self.stream,
            self.config,
            self.tx_allocator,
            self.rx_allocator,
        )
        .map_err(Into::into)
    }
}

pub struct GenericStreamAcceptor<A, B, TxA, RxA>
where
    A: Acceptor,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B>,
    RxA: BufferAllocator<Payload = B, SendMark = ()>,
{
    inner: A,
    config: PeerConfig,
    tx_allocator: TxA,
    rx_allocator: RxA,
    _marker: PhantomData<B>,
}

impl<A, B, TxA, RxA> TransportAcceptor<PeerSink<B>, PeerSource<BufGuard<B, RxA>>>
    for GenericStreamAcceptor<A, B, TxA, RxA>
where
    A: Acceptor,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B>,
    RxA: BufferAllocator<Payload = B, SendMark = ()>,
{
    type Transport = StreamTransport<A::Stream, B, TxA, RxA>;

    async fn accept(&self) -> anyhow::Result<Self::Transport> {
        let (stream, _) = self.inner.accept().await?;
        Ok(StreamTransport::new(
            stream,
            self.config.clone(),
            self.tx_allocator.clone(),
            self.rx_allocator.clone(),
        ))
    }
}

pub struct GenericStreamBuilder<AB, B, TxA, RxA>
where
    AB: AcceptorBuilder,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B>,
    RxA: BufferAllocator<Payload = B, SendMark = ()>,
{
    inner_builder: AB,
    config: PeerConfig,
    tx_allocator: TxA,
    rx_allocator: RxA,
    _marker: PhantomData<B>,
}

impl<AB, B, TxA, RxA> GenericStreamBuilder<AB, B, TxA, RxA>
where
    AB: AcceptorBuilder,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B>,
    RxA: BufferAllocator<Payload = B, SendMark = ()>,
{
    pub fn new(
        inner_builder: AB,
        config: PeerConfig,
        tx_allocator: TxA,
        rx_allocator: RxA,
    ) -> Self {
        Self {
            inner_builder,
            config,
            tx_allocator,
            rx_allocator,
            _marker: PhantomData,
        }
    }
}

impl<AB, B, TxA, RxA> TransportPerWorkerBuilder<PeerSink<B>, PeerSource<BufGuard<B, RxA>>>
    for GenericStreamBuilder<AB, B, TxA, RxA>
where
    AB: AcceptorBuilder,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B> + Send,
    RxA: BufferAllocator<Payload = B, SendMark = ()> + Send,
{
    type Transport = StreamTransport<AB::Stream, B, TxA, RxA>;
    type Acceptor = GenericStreamAcceptor<AB::Acceptor, B, TxA, RxA>;

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
            tx_allocator: self.tx_allocator,
            rx_allocator: self.rx_allocator,
            _marker: PhantomData,
        })
    }
}

pub struct GenericStreamConnector<C, B, TxA, RxA>
where
    C: Connector,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B>,
    RxA: BufferAllocator<Payload = B, SendMark = ()>,
{
    inner: C,
    config: PeerConfig,
    tx_allocator: TxA,
    rx_allocator: RxA,
    _marker: PhantomData<B>,
}

impl<C, B, TxA, RxA> GenericStreamConnector<C, B, TxA, RxA>
where
    C: Connector,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B>,
    RxA: BufferAllocator<Payload = B, SendMark = ()>,
{
    pub fn new(inner: C, config: PeerConfig, tx_allocator: TxA, rx_allocator: RxA) -> Self {
        Self {
            inner,
            config,
            tx_allocator,
            rx_allocator,
            _marker: PhantomData,
        }
    }
}

impl<C, B, TxA, RxA> TransportConnector<PeerSink<B>, PeerSource<BufGuard<B, RxA>>>
    for GenericStreamConnector<C, B, TxA, RxA>
where
    C: Connector,
    B: StreamBuf,
    TxA: BufferAllocator<Payload = B> + Send,
    RxA: BufferAllocator<Payload = B, SendMark = ()> + Send,
{
    type Transport = StreamTransport<C::Stream, B, TxA, RxA>;

    async fn connect(&self) -> anyhow::Result<Self::Transport> {
        let stream = self.inner.connect().await?;
        Ok(StreamTransport::new(
            stream,
            self.config.clone(),
            self.tx_allocator.clone(),
            self.rx_allocator.clone(),
        ))
    }
}
