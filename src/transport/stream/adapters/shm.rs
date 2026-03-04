use crate::align_buffer::AlignedBuffer;
use crate::transport::base::TransportBuilder;
use crate::transport::impls::shm::{
    IceoryxBuilder, IceoryxConnector, IceoryxSinkAdapter, IceoryxSourceAdapter,
};

#[derive(Clone)]
pub struct ShmTransport {
    base_name: String,
}

impl ShmTransport {
    pub fn new(base_name: &str) -> Self {
        Self {
            base_name: base_name.to_string(),
        }
    }
}

impl TransportBuilder<AlignedBuffer> for ShmTransport {
    type Si = IceoryxSinkAdapter;

    type So = IceoryxSourceAdapter;

    type Builder = IceoryxBuilder;
    type Connector = IceoryxConnector;

    fn kind(&self) -> String {
        "shm".to_string()
    }

    fn server_builder(&self) -> Self::Builder {
        IceoryxBuilder::new(&self.base_name)
    }

    fn client_connector(&self, endpoint: String) -> anyhow::Result<Self::Connector> {
        if endpoint.is_empty() {
            return Err(anyhow::anyhow!(
                "Iceoryx endpoint (service name) cannot be empty"
            ));
        }

        Ok(IceoryxConnector::new(&endpoint))
    }
}
