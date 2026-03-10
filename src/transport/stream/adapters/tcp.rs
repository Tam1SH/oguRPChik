use std::marker::PhantomData;
use num_cpus::get;
use crate::codecs::base::{BufferAllocator, HasAllocator, OwnedBuf};
use crate::transport::base::pool_config::PoolConfig;
use crate::transport::base::TransportBuilder;
use crate::transport::impls::peer::adapter::{GenericStreamBuilder, GenericStreamConnector};
use crate::transport::impls::peer::config::PeerConfig;
use crate::transport::impls::peer::handle::{PeerSink, PeerSource};
use crate::transport::stream::StreamBuf;
use crate::transport::stream::tcp::{TcpAcceptorBuilder, TcpConnector};

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

impl<B: StreamBuf + HasAllocator> TransportBuilder<B> for TcpTransport<B> {
    type Rx = B;
    type Si = PeerSink<B>;
    type So = PeerSource<B>;
    type Builder = GenericStreamBuilder<TcpAcceptorBuilder, B, <B as HasAllocator>::Alloc>;
    type Connector = GenericStreamConnector<TcpConnector, B, <B as HasAllocator>::SharedAlloc>;

    fn kind(&self) -> String {
        "tcp".to_string()
    }

    fn server_builder(&self, _: usize) -> Self::Builder {
        let addr = format!("{}:0", self.host);

        let temp_listener =
            std::net::TcpListener::bind(&addr).expect("Failed to bind to a temporary port");

        let actual_addr = temp_listener
            .local_addr()
            .expect("Failed to get local address");

        GenericStreamBuilder::new(TcpAcceptorBuilder::new(actual_addr), self.config.clone(), <<B as HasAllocator>::Alloc as BufferAllocator>::get(&PoolConfig::default()))
    }

    fn client_connector(&self, endpoint: String, core_id: usize) -> anyhow::Result<Self::Connector> {
        Ok(GenericStreamConnector::new(
            TcpConnector::new(endpoint.parse()?),
            self.config.clone(),
            <<B as HasAllocator>::SharedAlloc as BufferAllocator>::get(&PoolConfig::default()),
        ))
    }
}