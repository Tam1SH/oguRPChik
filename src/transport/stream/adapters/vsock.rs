use crate::codecs::base::{BufferAllocator, HasAllocator, OwnedBuf};
use crate::pool::buf_guard::BufGuard;
use crate::transport::base::TransportBuilder;
use crate::transport::base::pool_config::PoolConfig;
use crate::transport::impls::peer::adapter::{GenericStreamBuilder, GenericStreamConnector};
use crate::transport::impls::peer::config::PeerConfig;
use crate::transport::impls::peer::handle::{PeerSink, PeerSource};
use crate::transport::stream::StreamBuf;
use crate::transport::stream::vsock::{VsockAcceptorBuilder, VsockConnector, VsockTarget};
use std::marker::PhantomData;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tracing::{error, info, instrument, trace};
use uuid::Uuid;

type TxA<B> = <B as HasAllocator>::Alloc;
type RxA<B> = <B as HasAllocator>::SharedAlloc;

#[derive(Debug, Clone, Copy)]
pub enum VsockAddr {
    Cid(u32),
    Id(Uuid),
    SelfManaged,
}

impl FromStr for VsockAddr {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        const U32_MAX_STRING: &str = "4294967295";
        if s == U32_MAX_STRING || s == "any" || s == "self" {
            Ok(Self::SelfManaged)
        } else if let Ok(u) = Uuid::parse_str(s) {
            Ok(Self::Id(u))
        } else if let Ok(c) = s.parse::<u32>() {
            Ok(Self::Cid(c))
        } else {
            Err(anyhow::anyhow!("Invalid vsock addr format: {}", s))
        }
    }
}

#[derive(Clone)]
pub struct VsockTransport<B: OwnedBuf> {
    resolved_id: VsockTarget,
    base_port: u32,
    config: PeerConfig,
    port_offset: Arc<AtomicU32>,
    _marker: PhantomData<B>,
}

impl<B: OwnedBuf> VsockTransport<B> {
    pub fn server(addr: VsockAddr, base_port: u32) -> Self {
        Self::new(addr, Some(base_port))
    }
    pub fn client(addr: VsockAddr) -> Self {
        Self::new(addr, None)
    }

    fn new(addr: VsockAddr, base_port: Option<u32>) -> Self {
        let base_port = base_port.unwrap_or(0);
        let resolved_id = match addr {
            VsockAddr::Cid(c) => VsockTarget::Cid(c),
            VsockAddr::Id(uuid) => VsockTarget::Guid(uuid),
            VsockAddr::SelfManaged => {
                #[cfg(windows)]
                {
                    use crate::transport::stream::vsock::utils::*;
                    match get_best_vmid() {
                        Ok(u) => VsockTarget::Guid(guid_to_uuid(u)),
                        _ => VsockTarget::Cid(u32::MAX),
                    }
                }
                #[cfg(unix)]
                {
                    VsockTarget::Cid(u32::MAX)
                }
            }
        };

        info!(transport = "vsock", resolved_id = ?resolved_id, base_port, "Initializing VsockTransport");

        Self {
            resolved_id,
            base_port,
            config: PeerConfig::default(),
            port_offset: Arc::new(AtomicU32::new(0)),
            _marker: PhantomData,
        }
    }

    fn resolve_phys_target(&self, logical: VsockAddr) -> VsockTarget {
        match logical {
            VsockAddr::SelfManaged | VsockAddr::Id(_) => self.resolved_id,
            VsockAddr::Cid(c) => VsockTarget::Cid(c),
        }
    }
}

impl<B: StreamBuf + HasAllocator> TransportBuilder<B> for VsockTransport<B>
where
    TxA<B>: BufferAllocator<Payload = B>,
    RxA<B>: BufferAllocator<Payload = B, SendMark = ()>,
{
    type Rx = BufGuard<B, RxA<B>>;
    type Si = PeerSink<B>;
    type So = PeerSource<BufGuard<B, RxA<B>>>;
    type Builder = GenericStreamBuilder<VsockAcceptorBuilder, B, TxA<B>, RxA<B>>;
    type Connector = GenericStreamConnector<VsockConnector, B, TxA<B>, RxA<B>>;

    fn kind(&self) -> String {
        "vsock".to_string()
    }

    #[instrument(skip(self), fields(cid = ?self.resolved_id, base_port = self.base_port))]
    fn server_builder(&self, _: usize) -> Self::Builder {
        assert!(
            self.base_port > 0,
            "server_builder() called on client-side VsockTransport"
        );
        let port = self.base_port + self.port_offset.fetch_add(1, Ordering::SeqCst);
        GenericStreamBuilder::new(
            VsockAcceptorBuilder::new(self.resolved_id, port),
            self.config.clone(),
            TxA::<B>::get(&PoolConfig::default()),
            RxA::<B>::get(&PoolConfig::default()),
        )
    }

    #[instrument(skip(self), fields(endpoint = %endpoint))]
    fn client_connector(
        &self,
        endpoint: String,
        _core_id: usize,
    ) -> anyhow::Result<Self::Connector> {
        let (addr_str, port_str) = endpoint.rsplit_once(':').ok_or_else(|| {
            anyhow::anyhow!("Invalid format: expected ID:PORT, got '{}'", endpoint)
        })?;

        let logical_addr: VsockAddr = addr_str.parse()?;
        let port: u32 = port_str.parse()?;
        let physical_target = self.resolve_phys_target(logical_addr);

        info!(logical = ?logical_addr, physical = ?physical_target, port, "Vsock endpoint resolved");

        Ok(GenericStreamConnector::new(
            VsockConnector::new(physical_target, port),
            self.config.clone(),
            TxA::<B>::get(&PoolConfig::default()),
            RxA::<B>::get(&PoolConfig::default()),
        ))
    }
}
