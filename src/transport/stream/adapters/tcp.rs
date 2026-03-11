use crate::codecs::base::{BufferAllocator, HasAllocator, OwnedBuf};
use crate::pool::buf_guard::BufGuard;
use crate::transport::base::TransportBuilder;
use crate::transport::base::pool_config::PoolConfig;
use crate::transport::impls::peer::adapter::{GenericStreamBuilder, GenericStreamConnector};
use crate::transport::impls::peer::config::PeerConfig;
use crate::transport::impls::peer::handle::{PeerSink, PeerSource};
use crate::transport::stream::StreamBuf;
use crate::transport::stream::tcp::{TcpAcceptorBuilder, TcpConnector};
use std::marker::PhantomData;

type TxA<B> = <B as HasAllocator>::Alloc;
type RxA<B> = <B as HasAllocator>::SharedAlloc;

#[derive(Clone)]
pub struct TcpTransport<B: OwnedBuf> {
    host: String,
    config: PeerConfig,
    _marker: PhantomData<B>,
}

impl<B: OwnedBuf> TcpTransport<B> {
    pub fn new(host: String) -> Self {
        Self {
            host,
            config: PeerConfig::default(),
            _marker: PhantomData,
        }
    }
}

impl<B: StreamBuf + HasAllocator> TransportBuilder<B> for TcpTransport<B>
where
    TxA<B>: BufferAllocator<Payload = B>,
    RxA<B>: BufferAllocator<Payload = B, SendMark = ()>,
{
    type Rx = BufGuard<B, RxA<B>>;
    type Si = PeerSink<B>;
    type So = PeerSource<BufGuard<B, RxA<B>>>;
    type Builder = GenericStreamBuilder<TcpAcceptorBuilder, B, TxA<B>, RxA<B>>;
    type Connector = GenericStreamConnector<TcpConnector, B, TxA<B>, RxA<B>>;

    fn kind(&self) -> String {
        "tcp".to_string()
    }

    fn server_builder(&self, _: usize) -> Self::Builder {
        let addr = format!("{}:0", self.host);
        let listener =
            std::net::TcpListener::bind(&addr).expect("Failed to bind to a temporary port");
        let actual_addr = listener.local_addr().expect("Failed to get local address");

        GenericStreamBuilder::new(
            TcpAcceptorBuilder::new(actual_addr),
            self.config.clone(),
            TxA::<B>::get(&PoolConfig::default()),
            RxA::<B>::get(&PoolConfig::default()),
        )
    }

    fn client_connector(
        &self,
        endpoint: String,
        _core_id: usize,
    ) -> anyhow::Result<Self::Connector> {
        Ok(GenericStreamConnector::new(
            TcpConnector::new(endpoint.parse()?),
            self.config.clone(),
            TxA::<B>::get(&PoolConfig::default()),
            RxA::<B>::get(&PoolConfig::default()),
        ))
    }
}
