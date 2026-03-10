use crate::codecs::base::{BorrowedBuf, OwnedBuf};
use crate::transport::base::TransportBuilder;
use crate::transport::impls::shm::{IceoryxBuilder, IceoryxConnector, IceoryxPayload, IceoryxSinkAdapter, IceoryxSourceAdapter};

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

impl<B: OwnedBuf> TransportBuilder<B> for ShmTransport {
    type Rx = IceoryxPayload;
    type Si = IceoryxSinkAdapter<B>;

    type So = IceoryxSourceAdapter;

    type Builder = IceoryxBuilder<B>;
    type Connector = IceoryxConnector<B>;

    fn kind(&self) -> String {
        "shm".to_string()
    }

    fn server_builder(&self, _: usize) -> Self::Builder {
        IceoryxBuilder::new(&self.base_name)
    }

    fn client_connector(&self, endpoint: String, _: usize) -> anyhow::Result<Self::Connector> {
        if endpoint.is_empty() {
            return Err(anyhow::anyhow!(
                "Iceoryx endpoint (service name) cannot be empty"
            ));
        }

        Ok(IceoryxConnector::new(&endpoint))
    }
}
