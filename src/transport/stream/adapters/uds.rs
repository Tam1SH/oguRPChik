use crate::codecs::base::{BufferAllocator, HasAllocator, OwnedBuf};
use crate::pool::buf_guard::BufGuard;
use crate::transport::base::TransportBuilder;
use crate::transport::base::pool_config::PoolConfig;
use crate::transport::impls::peer::adapter::{GenericStreamBuilder, GenericStreamConnector};
use crate::transport::impls::peer::config::PeerConfig;
use crate::transport::impls::peer::handle::{PeerSink, PeerSource};
use crate::transport::stream::StreamBuf;
use crate::transport::stream::uds::{UdsAcceptorBuilder, UdsConnector};
use std::marker::PhantomData;
use std::path::PathBuf;

type TxA<B> = <B as HasAllocator>::Alloc;
type RxA<B> = <B as HasAllocator>::SharedAlloc;

#[derive(Clone)]
pub struct UdsTransport<B: OwnedBuf> {
    base_path: PathBuf,
    config: PeerConfig,
    _marker: PhantomData<B>,
}

impl<B: OwnedBuf> UdsTransport<B> {
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
            config: PeerConfig::default(),
            _marker: PhantomData,
        }
    }

    pub fn temp(name: &str) -> Self {
        Self::new(std::env::temp_dir().join(name))
    }
}

impl<B: StreamBuf + HasAllocator> TransportBuilder<B> for UdsTransport<B>
where
    TxA<B>: BufferAllocator<Payload = B>,
    RxA<B>: BufferAllocator<Payload = B, SendMark = ()>,
{
    type Rx = BufGuard<B, RxA<B>>;
    type Si = PeerSink<B>;
    type So = PeerSource<BufGuard<B, RxA<B>>>;
    type Builder = GenericStreamBuilder<UdsAcceptorBuilder, B, TxA<B>, RxA<B>>;
    type Connector = GenericStreamConnector<UdsConnector, B, TxA<B>, RxA<B>>;

    fn kind(&self) -> String {
        "uds".to_string()
    }

    fn server_builder(&self, core_id: usize) -> Self::Builder {
        let path = self.base_path.with_extension(format!("{}.sock", core_id));
        GenericStreamBuilder::new(
            UdsAcceptorBuilder::new(path),
            self.config.clone(),
            TxA::<B>::get(&PoolConfig::default()),
            RxA::<B>::get(&PoolConfig::default()),
        )
    }

    fn client_connector(&self, endpoint: String, _core_id: usize) -> anyhow::Result<Self::Connector> {
        Ok(GenericStreamConnector::new(
            UdsConnector::new(PathBuf::from(endpoint)),
            self.config.clone(),
            TxA::<B>::get(&PoolConfig::default()),
            RxA::<B>::get(&PoolConfig::default()),
        ))
    }
}