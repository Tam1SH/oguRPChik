use std::marker::PhantomData;
use crate::client_per_core::{ClientPerCore, ResponseGuard};
use crate::codecs::base::MessageCodec;
use crate::runtime;
use crate::transport::stream::Connector;
use anyhow::{anyhow, Result};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};
use crate::discovery::Topology;
use crate::server::HasDefaultAllocator;
use crate::transport::base::{MessageSink, MessageSource, TransportBuilder, TransportConnector};

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
    workers: Rc<Vec<flume::Sender<CallRequest<C, P>>>>,
    rr_idx: Rc<AtomicUsize>,
}

impl<C: MessageCodec, P: AsRef<[u8]> + Send + 'static> Clone for Client<C, P> {
    fn clone(&self) -> Self {
        Self {
            workers: self.workers.clone(),
            rr_idx: self.rr_idx.clone(),
        }
    }
}


impl<C, P> Client<C, P>
where
    C: MessageCodec<Dest = P> + 'static,
    P: AsRef<[u8]> + Send + HasDefaultAllocator + 'static,
{
    #[instrument(
        level = "info",
        skip(transport, topology),
        fields(
            transport = %transport.kind(),
            num_cores = topology.map.len(),
            expected_codec = %C::kind()
        )
    )]
    pub async fn connect<T>(transport: T, topology: Topology) -> Result<Self>
    where
        T: TransportBuilder<P>,
    {
        info!("Starting client connection process");

        if topology.transport_kind != transport.kind() {
            let err = format!(
                "Topology transport mismatch: registry has '{}', but transport is '{}'",
                topology.transport_kind,
                transport.kind()
            );
            error!("{}", err);
            return Err(anyhow!(err));
        }

        if topology.codec_kind != C::kind() {
            let err = format!(
                "Codec mismatch: registry expects '{}', but client uses '{}'",
                topology.codec_kind,
                C::kind()
            );
            error!("{}", err);
            return Err(anyhow!(err));
        }

        let num_cores = topology.map.len();
        let mut connectors = Vec::with_capacity(num_cores);

        debug!("Building connectors for {} cores", num_cores);

        for i in 0..num_cores {
            let endpoint = topology.map.get(&i).ok_or_else(|| {
                let err = format!("Core {} not found in topology map", i);
                error!("{}", err);
                anyhow!(err)
            })?;

            debug!(core_id = i, endpoint = %endpoint, "Creating connector");

            match transport.client_connector(endpoint.clone()) {
                Ok(connector) => connectors.push(connector),
                Err(e) => {
                    error!(core_id = i, endpoint = %endpoint, error = %e, "Failed to create connector");
                    return Err(e);
                }
            }
        }

        info!("All connectors prepared, initiating connect_internal");

        match Self::connect_internal(connectors, num_cores).await {
            Ok(client) => {
                info!("Client successfully connected to all cores");
                Ok(client)
            },
            Err(e) => {
                error!(error = %e, "Internal connection failed");
                Err(e)
            }
        }
    }
    #[instrument(skip(connectors))]
    async fn connect_internal<Conn, Si, So>(
        connectors: Vec<Conn>,
        num_cores: usize
    ) -> Result<Self>
    where
        Si: MessageSink<Payload = C::Dest> + Clone + 'static,
        So: MessageSource<Payload = C::Dest> + 'static,
        Conn: TransportConnector<Si, So>,
    {

        runtime::init(num_cores);

        let mut workers = Vec::with_capacity(num_cores);
        let (init_tx, init_rx) = flume::bounded::<Result<usize>>(num_cores);

        for (core_id, connector) in connectors.into_iter().enumerate() {
            let (worker_tx, worker_rx) = flume::unbounded::<CallRequest<C, P>>();
            workers.push(worker_tx);

            let sync_tx = init_tx.clone();

            runtime::spawn_on(core_id, move || async move {

                let connect_res = async {
                    let transport = connector.connect().await?;
                    ClientPerCore::<C, Si, So, <C::Dest as HasDefaultAllocator>::Alloc>::connect(
                        transport
                    ).await
                }.await;

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

        info!(num_workers = workers.len(), "Client pool initialized");

        Ok(Self {
            workers: Rc::new(workers),
            rr_idx: Rc::new(AtomicUsize::new(0)),
        })
    }

    pub async fn call(&self, req: C::Request) -> Result<ResponseGuard<C::Dest, C>> {
        
        let idx = self.rr_idx.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        let tx = &self.workers[idx];
        
        let (resp_tx, resp_rx) = oneshot::channel();

        if let Err(e) = tx.send_async(CallRequest { req, resp_tx }).await {
            error!(error = %e, "Failed to send request: worker task died");
            return Err(anyhow!("Worker task dropped"));
        }

        resp_rx.await.map_err(|e| {
            error!(error = %e, "Worker failed to provide response (oneshot closed)");
            anyhow!("Worker response cancelled")
        })?
    }
}
