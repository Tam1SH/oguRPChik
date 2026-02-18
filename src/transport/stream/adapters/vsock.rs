use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use crate::align_buffer::AlignedBuffer;
use crate::transport::impls::peer::handle::{PeerSink, PeerSource};
use crate::transport::impls::peer::adapter::{GenericStreamBuilder, GenericStreamConnector};
use crate::transport::base::TransportBuilder;
use crate::transport::impls::peer::config::PeerConfig;
use crate::transport::stream::vsock::{VsockAcceptorBuilder, VsockConnector};

#[derive(Clone)]
pub struct VsockTransport {
    cid: u32,
    base_port: u32,
    config: PeerConfig,
    port_offset: Arc<AtomicU32>,
}

impl VsockTransport {
    pub fn new(cid: u32, base_port: u32, config: PeerConfig) -> Self {
        Self {
            cid,
            base_port,
            config,
            port_offset: Arc::new(AtomicU32::new(0)),
        }
    }
}


impl TransportBuilder<AlignedBuffer> for VsockTransport {
    type Si = PeerSink;
    type So = PeerSource;
    type Builder = GenericStreamBuilder<VsockAcceptorBuilder>;
    type Connector = GenericStreamConnector<VsockConnector>;

    fn kind(&self) -> String {
        "vsock".to_string()
    }

    fn server_builder(&self) -> Self::Builder {

        let offset = self.port_offset.fetch_add(1, Ordering::SeqCst);
        let port = self.base_port + offset;

        GenericStreamBuilder::new(
            VsockAcceptorBuilder::new(self.cid, port),
            self.config.clone(),
        )
    }

    fn client_connector(&self, endpoint: String) -> anyhow::Result<Self::Connector> {

        let parts: Vec<&str> = endpoint.split(':').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!("Invalid vsock endpoint format. Expected 'cid:port', got: {}", endpoint));
        }

        let cid: u32 = parts[0].parse()?;
        let port: u32 = parts[1].parse()?;

        Ok(GenericStreamConnector::new(
            VsockConnector::new(cid, port),
            self.config.clone(),
        ))
    }
}
