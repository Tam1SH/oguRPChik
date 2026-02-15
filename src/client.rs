use std::marker::PhantomData;
use crate::client_per_core::{ClientPerCore, ResponseGuard};
use crate::message_codec::MessageCodec;
use crate::runtime;
use crate::transport::stream::Connector;
use anyhow::{anyhow, Result};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use crate::server::HasDefaultAllocator;
use crate::transport::base::{MessageSink, MessageSource, TransportBuilder, TransportConnector};
use crate::transport::topology::Topology;

#[derive(Clone, Copy, Debug)]
pub enum Priority {
    Critical,
    Normal,
    Bulk,
}

struct CallRequest<C: MessageCodec, P: AsRef<[u8]> + Send + 'static> {
    req: C::Request,
    resp_tx: oneshot::Sender<Result<ResponseGuard<P, C>>>,
}

pub struct Client<C: MessageCodec, P: AsRef<[u8]> + Send + 'static> {
    critical_lane: flume::Sender<CallRequest<C, P>>,
    normal_lanes: Rc<Vec<flume::Sender<CallRequest<C, P>>>>,
    rr_normal: Rc<AtomicUsize>,
    bulk_lanes: Rc<Vec<flume::Sender<CallRequest<C, P>>>>,
    rr_bulk: Rc<AtomicUsize>,
}

impl<C: MessageCodec, P: AsRef<[u8]> + Send + 'static> Clone for Client<C, P> {
    fn clone(&self) -> Self {
        Self {
            bulk_lanes: self.bulk_lanes.clone(),
            critical_lane: self.critical_lane.clone(),
            normal_lanes: self.normal_lanes.clone(),
            rr_bulk: self.rr_bulk.clone(),
            rr_normal: self.rr_normal.clone(),
        }
    }
}


impl<C, P> Client<C, P>
where
    C: MessageCodec<Dest = P> + 'static,
    P: AsRef<[u8]> + Send + HasDefaultAllocator + 'static,
{
    pub async fn connect<T>(transport: T, topology: Topology) -> Result<Self>
    where
        T: TransportBuilder<P>,
    {
        if topology.transport_kind != transport.kind() {
            return Err(anyhow!("Topology kind mismatch: {} vs {}", topology.transport_kind, transport.kind()));
        }

        if topology.codec_kind != C::kind() {
            return Err(anyhow!("Codec kind mismatch: {} vs {}", topology.codec_kind, C::kind()));
        }

        let num_cores = topology.map.len();

        let mut connectors = Vec::with_capacity(num_cores);

        for i in 0..num_cores {
            let endpoint = topology.map.get(&i)
                .ok_or_else(|| anyhow!("Core {} not found in topology", i))?;
            connectors.push(transport.client_connector(endpoint.clone())?);
        }

        Self::connect_internal(connectors, num_cores).await
    }

    async fn connect_internal<Conn, Si, So>(
        connectors: Vec<Conn>,
        limit_cores: usize,
    ) -> Result<Self>
    where
        Si: MessageSink<Payload = C::Dest> + Clone + 'static,
        So: MessageSource<Payload = C::Dest> + 'static,
        Conn: TransportConnector<Si, So>,
    {
        let available_runtime_cores = runtime::core_count();

        let num_cores = limit_cores.min(available_runtime_cores);

        assert!(num_cores >= 1, "At least 1 core required for client");

        let mut all_worker_txs = Vec::with_capacity(num_cores);
        let (init_tx, init_rx) = flume::bounded::<Result<usize>>(num_cores);

        for (core_id, connector) in connectors.into_iter().enumerate() {
            let (worker_tx, worker_rx) = flume::unbounded::<CallRequest<C, P>>();
            all_worker_txs.push(worker_tx);

            let sync_tx = init_tx.clone();

            runtime::spawn_on(core_id, move || async move {
                let connect_res = async {
                    let transport = connector.connect().await?;

                    ClientPerCore::<C, Si, So, <C::Dest as HasDefaultAllocator>::Alloc>::connect(transport).await
                }
                    .await;


                match connect_res {
                    Ok(mut client) => {
                        debug!(core_id, "Worker connected successfully");
                        let _ = sync_tx.send_async(Ok(core_id)).await;

                        while let Ok(msg) = worker_rx.recv_async().await {
                            let res = client.call(msg.req).await;
                            if msg.resp_tx.send(res).is_err() {
                                warn!(
                                    core_id,
                                    "Caller dropped response channel before receiving result"
                                );
                            }
                        }
                        info!(core_id, "Worker shutting down (channel closed)");
                    }
                    Err(e) => {
                        error!(core_id, error = %e, "Worker failed to connect");
                        let _ = sync_tx.send_async(Err(e)).await;
                    }
                }
            });
        }

        for _ in 0..num_cores {
            match init_rx.recv_async().await {
                Ok(Ok(core_id)) => {
                    debug!(core_id, "Worker sync successfully");
                }
                Ok(Err(e)) => {
                    error!(error = %e, "Failed to initialize one or more core workers");
                    return Err(e);
                }
                Err(_) => {
                    return Err(anyhow!("Init channel closed prematurely"));
                }
            }
        }

        let critical_lane = all_worker_txs[0].clone();
        let remaining_workers = &all_worker_txs[1..];
        let count = remaining_workers.len();

        let (normal_workers, bulk_workers) = if count == 1 {
            (
                vec![remaining_workers[0].clone()],
                vec![remaining_workers[0].clone()],
            )
        } else {
            let mid = (count + 1) / 2;
            (
                remaining_workers[0..mid].to_vec(),
                remaining_workers[mid..].to_vec(),
            )
        };

        info!(
            critical = 1,
            normal = normal_workers.len(),
            bulk = bulk_workers.len(),
            "FatClient pool distribution complete"
        );

        Ok(Self {
            critical_lane,
            normal_lanes: Rc::new(normal_workers),
            rr_normal: Rc::new(AtomicUsize::new(0)),
            bulk_lanes: Rc::new(bulk_workers),
            rr_bulk: Rc::new(AtomicUsize::new(0)),
        })
    }

    pub async fn call(&self, req: C::Request, prio: Priority) -> Result<ResponseGuard<C::Dest, C>> {
        let tx = match prio {
            Priority::Critical => &self.critical_lane,
            Priority::Normal => {
                let idx = self.rr_normal.fetch_add(1, Ordering::Relaxed) % self.normal_lanes.len();
                &self.normal_lanes[idx]
            }
            Priority::Bulk => {
                let idx = self.rr_bulk.fetch_add(1, Ordering::Relaxed) % self.bulk_lanes.len();
                &self.bulk_lanes[idx]
            }
        };

        let (resp_tx, resp_rx) = oneshot::channel();

        if let Err(_) = tx.send_async(CallRequest { req, resp_tx }).await {
            error!(?prio, "Failed to send request: worker task died");
            return Err(anyhow!("Worker task dropped"));
        }

        resp_rx.await.map_err(|e| {
            error!(?prio, error = %e, "Worker failed to provide response (oneshot closed)");
            anyhow!("Worker response cancelled")
        })?
    }
}
