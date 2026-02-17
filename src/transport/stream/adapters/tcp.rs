use crate::align_buffer::AlignedBuffer;
use crate::server::HasDefaultAllocator;
use crate::transport::base::stream::{GenericStreamBuilder, GenericStreamConnector};
use crate::transport::base::{MessageSink, MessageSource, TransportBuilder, TransportConnector, TransportPerWorkerBuilder};
use crate::transport::base::handle::{PeerSink, PeerSource};
use crate::transport::impls::peer::PeerConfig;
use crate::transport::stream::tcp::{TcpAcceptorBuilder, TcpConnector};

#[derive(Clone)]
pub struct TcpTransport {
    host: String,
    config: PeerConfig,
}

impl TcpTransport {
    pub fn new(host: String, config: PeerConfig) -> Self {
        Self {
            host,
            config
        }
    }
}

impl TransportBuilder<AlignedBuffer> for TcpTransport
{
    type Si = PeerSink;
    type So = PeerSource;
    type Builder = GenericStreamBuilder<TcpAcceptorBuilder>;
    type Connector = GenericStreamConnector<TcpConnector>;

    fn kind(&self) -> String { "tcp".to_string() }

    fn server_builder(&self) -> Self::Builder {
        let addr = format!("{}:0", self.host);

        let temp_listener = std::net::TcpListener::bind(&addr)
            .expect("Failed to bind to a temporary port");

        let actual_addr = temp_listener.local_addr()
            .expect("Failed to get local address");

        GenericStreamBuilder::new(
            TcpAcceptorBuilder::new(actual_addr),
            self.config.clone(),
        )
    }

    fn client_connector(&self, endpoint: String) -> anyhow::Result<Self::Connector> {
        Ok(GenericStreamConnector::new(
            TcpConnector::new(endpoint.parse()?),
            self.config.clone(),
        ))
    }
}