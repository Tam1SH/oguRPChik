use crate::codecs::base::{BufferAllocator, HasAllocator, OwnedBuf};
use crate::pool::buf_guard::BufGuard;
use crate::transport::base::TransportBuilder;
use crate::transport::base::pool_config::PoolConfig;
use crate::transport::impls::peer::adapter::{GenericStreamBuilder, GenericStreamConnector};
use crate::transport::impls::peer::config::PeerConfig;
use crate::transport::impls::peer::handle::{PeerSink, PeerSource};
use crate::transport::stream::StreamBuf;
use crate::transport::stream::npipe::{NamedPipeAcceptorBuilder, NamedPipeConnector, NamedPipePath};
use std::marker::PhantomData;

type TxA<B> = <B as HasAllocator>::Alloc;
type RxA<B> = <B as HasAllocator>::SharedAlloc;

#[derive(Clone)]
pub struct NamedPipeTransport<B: OwnedBuf> {
    base_path: NamedPipePath,
    config: PeerConfig,
    _marker: PhantomData<B>,
}

impl<B: OwnedBuf> NamedPipeTransport<B> {
    pub fn new(base_path: impl Into<String>) -> Self {
        Self {
            base_path: NamedPipePath::new(base_path),
            config: PeerConfig::default(),
            _marker: PhantomData,
        }
    }

    pub fn temp(name: &str) -> Self {
        Self::new(name)
    }

    fn worker_path(&self, core_id: usize) -> String {
        format!("{}-{}", self.base_path, core_id)
    }
}

impl<B: StreamBuf + HasAllocator> TransportBuilder<B> for NamedPipeTransport<B>
where
    TxA<B>: BufferAllocator<Payload = B>,
    RxA<B>: BufferAllocator<Payload = B, SendMark = ()>,
{
    type Rx = BufGuard<B, RxA<B>>;
    type Si = PeerSink<B>;
    type So = PeerSource<BufGuard<B, RxA<B>>>;
    type Builder = GenericStreamBuilder<NamedPipeAcceptorBuilder, B, TxA<B>, RxA<B>>;
    type Connector = GenericStreamConnector<NamedPipeConnector, B, TxA<B>, RxA<B>>;

    fn kind(&self) -> String {
        "npipe".to_string()
    }

    fn server_builder(&self, core_id: usize) -> Self::Builder {
        GenericStreamBuilder::new(
            NamedPipeAcceptorBuilder::new(self.worker_path(core_id)),
            self.config.clone(),
            TxA::<B>::get(&PoolConfig::default()),
            RxA::<B>::get(&PoolConfig::default()),
        )
    }

    fn client_connector(&self, endpoint: String, _core_id: usize) -> anyhow::Result<Self::Connector> {
        Ok(GenericStreamConnector::new(
            NamedPipeConnector::new(endpoint),
            self.config.clone(),
            TxA::<B>::get(&PoolConfig::default()),
            RxA::<B>::get(&PoolConfig::default()),
        ))
    }
}
