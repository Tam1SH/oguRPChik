use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tracing::{debug, error, info, instrument, trace};
use uuid::Uuid;
use crate::align_buffer::AlignedBuffer;
use crate::transport::impls::peer::handle::{PeerSink, PeerSource};
use crate::transport::impls::peer::adapter::{GenericStreamBuilder, GenericStreamConnector};
use crate::transport::base::TransportBuilder;
use crate::transport::impls::peer::config::PeerConfig;
use crate::transport::stream::vsock::{VsockAcceptorBuilder, VsockConnector, VsockTarget};

#[derive(Debug, Clone, Copy)]
pub enum VsockAddr {
    Cid(u32),
    Id(Uuid),
    SelfManaged,
}

impl FromStr for VsockAddr {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        const U32_MAX_STRING: &'static str = "4294967295";

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
pub struct VsockTransport {
    resolved_id: VsockTarget,
    base_port: u32,
    config: PeerConfig,
    port_offset: Arc<AtomicU32>,
}

impl VsockTransport {
    pub fn server(cid: u32, base_port: u32) -> Self {
        Self::new(cid, Some(base_port))
    }

    pub fn client(cid: u32) -> Self {
        Self::new(cid, None)
    }

    fn new(cid: u32, base_port: Option<u32>) -> Self {

        let base_port = base_port.unwrap_or(0);

        let resolved_id = if cid == u32::MAX {
            #[cfg(windows)]
            {
                use crate::transport::stream::vsock::utils::*;
                match get_best_vmid() {
                    Ok(u) => VsockTarget::Guid(guid_to_uuid(u)),
                    _ => VsockTarget::Cid(u32::MAX),
                }
            }
            #[cfg(unix)]
            { VsockTarget::Cid(u32::MAX) }
        } else {
            VsockTarget::Cid(cid)
        };

        info!(
            transport = "vsock",
            resolved_id = ?resolved_id,
            base_port = base_port,
            "Initializing VsockTransport"
        );

        Self {
            resolved_id,
            base_port,
            config: PeerConfig::default(),
            port_offset: Arc::new(AtomicU32::new(0)),
        }
    }

    fn resolve_phys_target(&self, logical: VsockAddr) -> VsockTarget {
        match logical {
            VsockAddr::SelfManaged | VsockAddr::Id(_) => self.resolved_id,
            VsockAddr::Cid(c) => VsockTarget::Cid(c),
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

    #[instrument(skip(self), fields(cid = ?self.resolved_id, base_port = self.base_port))]
    fn server_builder(&self) -> Self::Builder {
        assert!(self.base_port > 0, "server_builder() called on client-side VsockTransport");
        let offset = self.port_offset.fetch_add(1, Ordering::SeqCst);
        let port = self.base_port + offset;

        GenericStreamBuilder::new(
            VsockAcceptorBuilder::new(self.resolved_id, port),
            self.config.clone(),
        )
    }

    #[instrument(skip(self), fields(endpoint = %endpoint, transport_default = ?self.resolved_id))]
    fn client_connector(&self, endpoint: String) -> anyhow::Result<Self::Connector> {
        trace!("Starting client connector resolution");

        let (addr_str, port_str) = endpoint.rsplit_once(':')
            .ok_or_else(|| {
                let err = anyhow::anyhow!("Invalid format: expected ID:PORT, got '{}'", endpoint);
                error!(error = %err);
                err
            })?;


        let logical_addr: VsockAddr = addr_str.parse().map_err(|e| {
            error!(addr = %addr_str, "Failed to parse VSOCK address: {}", e);
            anyhow::anyhow!("Address parse error: {}", e)
        })?;


        let port: u32 = port_str.parse().map_err(|e| {
            error!(port = %port_str, "Failed to parse port: {}", e);
            anyhow::anyhow!("Port parse error: {}", e)
        })?;

        let physical_target = self.resolve_phys_target(logical_addr);

        info!(
            logical = ?logical_addr,
            physical = ?physical_target,
            port = port,
            "Vsock endpoint resolved"
        );

        Ok(GenericStreamConnector::new(
            VsockConnector::new(physical_target, port),
            self.config.clone(),
        ))
    }
}
