use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use crate::discovery::kv::{default_kv, KvStore};
use crate::transport::base::TopologyRegistry;

pub mod kv;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topology {
    pub transport_kind: String,
    pub codec_kind: String,
    pub map: HashMap<usize, String>,
}

impl Topology {
    pub fn publish(&self, service_name: &str, kv: &dyn KvStore) -> anyhow::Result<()> {
        let json = serde_json::to_string(self)?;
        kv.write(&format!("Services\\{}", service_name), &json)?;
        tracing::info!(service = service_name, "Topology published");
        Ok(())
    }

    pub fn resolve(service_name: &str, kv: &dyn KvStore) -> anyhow::Result<Self> {
        let json = kv.read(&format!("Services\\{}", service_name))?;
        let topology = serde_json::from_str(&json)?;
        tracing::info!(service = service_name, "Topology resolved");
        Ok(topology)
    }

    pub fn resolve_default(service_name: &str) -> anyhow::Result<Self> {
        Self::resolve(service_name, &default_kv()?)
    }

    pub fn watch(
        service_name: &str,
        kv: Arc<dyn KvStore>,
    ) -> anyhow::Result<flume::Receiver<()>> {
        kv.watch(&format!("Services\\{}", service_name))
    }
}

pub struct ServiceRegistration {
    service_name: String,
    kv: Arc<dyn KvStore>,
}

impl Drop for ServiceRegistration {
    fn drop(&mut self) {
        let key = format!("Services\\{}", self.service_name);
        if let Err(e) = self.kv.delete(&key) {
            tracing::warn!("Failed to cleanup registry key '{}': {}", key, e);
        } else {
            tracing::info!(service = %self.service_name, "Registry key cleaned up");
        }
    }
}

#[cfg(all(target_os = "windows", feature = "vsock"))]
pub fn register_vm(vm_name: &str, kv: &dyn KvStore) -> anyhow::Result<()> {
    use crate::transport::stream::vsock::utils::{guid_to_uuid, get_best_vmid};
    let vmid = get_best_vmid()?;
    let uuid = guid_to_uuid(vmid);
    kv.write(&format!("Hosts\\{}", vm_name), &uuid.to_string())?;
    tracing::info!(vm = vm_name, vmid = %uuid, "VM registered");
    Ok(())
}

#[cfg(all(target_os = "windows", feature = "vsock"))]
pub fn register_vm_default(vm_name: &str) -> anyhow::Result<()> {
    register_vm(vm_name, &default_kv()?)
}

#[cfg(feature = "vsock")]
pub fn read_self_vmid(vm_name: &str, kv: &dyn KvStore) -> anyhow::Result<uuid::Uuid> {
    let s = kv.read(&format!("Hosts\\{}", vm_name))?;
    Ok(uuid::Uuid::parse_str(s.trim())?)
}

pub struct RpcTopologyRegistry {
    expected_connects: AtomicUsize,
    transport_kind: String,
    codec_kind: String,
    endpoints: DashMap<usize, String>,
    tx: Mutex<Option<oneshot::Sender<Topology>>>,
    rx: Mutex<Option<oneshot::Receiver<Topology>>>,
    publish_as: Option<(String, Arc<dyn KvStore>)>,
    registration: Mutex<Option<ServiceRegistration>>,
}

pub struct RpcTopologyRegistryBuilder {
    transport_kind: String,
    codec_kind: String,
    publish_as: Option<String>,
    kv: Option<Arc<dyn KvStore>>,
}

impl RpcTopologyRegistryBuilder {
    pub fn new(transport_kind: impl Into<String>, codec_kind: impl Into<String>) -> Self {
        Self {
            transport_kind: transport_kind.into(),
            codec_kind: codec_kind.into(),
            publish_as: None,
            kv: None,
        }
    }

    pub fn publish_as(mut self, service_name: impl Into<String>) -> Self {
        self.publish_as = Some(service_name.into());
        self
    }

    pub fn kv(mut self, kv: Arc<dyn KvStore>) -> Self {
        self.kv = Some(kv);
        self
    }

    pub fn build(self) -> anyhow::Result<Arc<RpcTopologyRegistry>> {
        let (tx, rx) = oneshot::channel();

        let publish_as = match self.publish_as {
            Some(name) => {
                let kv = match self.kv {
                    Some(kv) => kv,
                    None => Arc::new(default_kv()?)
                };

                Some((name, kv))
            }
            None => None,
        };

        Ok(Arc::new(RpcTopologyRegistry {
            expected_connects: AtomicUsize::new(0),
            transport_kind: self.transport_kind,
            codec_kind: self.codec_kind,
            endpoints: DashMap::new(),
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(Some(rx)),
            publish_as,
            registration: Mutex::new(None),
        }))
    }
}

impl RpcTopologyRegistry {
    pub fn builder(
        transport_kind: impl Into<String>,
        codec_kind: impl Into<String>,
    ) -> RpcTopologyRegistryBuilder {
        RpcTopologyRegistryBuilder::new(transport_kind, codec_kind)
    }

    pub async fn ready(&self) -> (Topology, Option<ServiceRegistration>) {
        let rx = self.rx.lock().unwrap().take()
            .expect("ready() called more than once");
        let topology = rx.await.expect("Topology sender dropped");
        let registration = self.registration.lock().unwrap().take();
        (topology, registration)
    }

    fn try_send_topology(&self) {
        let expected = self.expected_connects.load(Ordering::Acquire);
        if expected > 0 && self.endpoints.len() == expected {
            if let Some(tx) = self.tx.lock().unwrap().take() {
                let map = self.endpoints.iter()
                    .map(|r| (*r.key(), r.value().clone()))
                    .collect();

                let topology = Topology {
                    transport_kind: self.transport_kind.clone(),
                    codec_kind: self.codec_kind.clone(),
                    map,
                };

                if let Some((name, kv)) = &self.publish_as {
                    match topology.publish(name, kv.as_ref()) {
                        Ok(()) => {
                            *self.registration.lock().unwrap() = Some(ServiceRegistration {
                                service_name: name.clone(),
                                kv: kv.clone(),
                            });
                        }
                        Err(e) => tracing::warn!("Failed to publish topology '{}': {}", name, e),
                    }
                }

                let _ = tx.send(topology);
            }
        }
    }
}

impl Clone for RpcTopologyRegistry {
    fn clone(&self) -> Self {
        let (tx, rx) = oneshot::channel();
        Self {
            expected_connects: AtomicUsize::new(
                self.expected_connects.load(Ordering::Relaxed)
            ),
            transport_kind: self.transport_kind.clone(),
            codec_kind: self.codec_kind.clone(),
            endpoints: DashMap::new(),
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(Some(rx)),
            publish_as: self.publish_as.clone(),
            registration: Mutex::new(None),
        }
    }
}

impl TopologyRegistry for RpcTopologyRegistry {
    fn init_cores(&self, cores: usize) {
        self.expected_connects.store(cores, Ordering::Release);
    }

    fn register(&self, core_id: usize, endpoint: String) {
        self.endpoints.insert(core_id, endpoint);
        self.try_send_topology();
    }

    fn transport_name(&self) -> &str { &self.transport_kind }
    fn codec_name(&self) -> &str { &self.codec_kind }
}