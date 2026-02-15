use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use crate::transport::base::{Endpoint, TopologyRegistry};

pub struct RpcTopologyRegistry {
    expected_cores: usize,
    transport_kind: String,
    codec_kind: String,
    endpoints: DashMap<usize, String>,
    tx: Mutex<Option<oneshot::Sender<Topology>>>,
    rx: Mutex<Option<oneshot::Receiver<Topology>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topology {
    pub transport_kind: String,
    pub codec_kind: String,
    pub map: HashMap<usize, String>,
}

impl RpcTopologyRegistry {
    pub fn new(expected_cores: usize, transport_kind: String, codec_kind: String) -> Arc<Self> {
        let (tx, rx) = oneshot::channel();
        Arc::new(Self {
            expected_cores,
            transport_kind,
            codec_kind,
            endpoints: DashMap::new(),
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(Some(rx)),
        })
    }

    pub async fn ready(&self) -> Topology {
        let rx = self.rx.lock().unwrap().take()
            .expect("ready() can be called only once");

        rx.await.expect("Topology production failed - sender dropped")
    }
}

impl TopologyRegistry for RpcTopologyRegistry {
    fn register(&self, core_id: usize, endpoint: String) {
        self.endpoints.insert(core_id, endpoint);

        if self.endpoints.len() == self.expected_cores {
            if let Some(tx) = self.tx.lock().unwrap().take() {
                let map = self.endpoints.iter().map(|r| (*r.key(), r.value().clone())).collect();
                let _ = tx.send(Topology {
                    transport_kind: self.transport_kind.clone(),
                    codec_kind: self.codec_kind.clone(),
                    map
                });
            }
        }
    }

    fn transport_name(&self) -> &str {
        &self.transport_kind
    }

    fn codec_name(&self) -> &str {
        &self.codec_kind
    }
}