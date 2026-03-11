use crate::codecs::base::{BufferAllocator, HasAllocator, MessageCodec, OwnedBuf, ReleasableBuf};
use crate::discovery::Topology;
use crate::low::client_per_core::{ClientConfig, ClientPerCore, ResponseGuard};
use crate::low::runtime;
use crate::transport::base::pool_config::PoolConfig;
use crate::transport::base::{MessageSink, MessageSource, TransportBuilder, TransportConnector};
use anyhow::{Result, anyhow};
use compio::time::timeout;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tracing::{debug, error, info, instrument, warn};

type TxAlloc<Tx> = <Tx as HasAllocator>::Alloc;

struct CallRequest<C, Rx>
where
    C: MessageCodec,
    Rx: ReleasableBuf,
{
    req: C::Request,
    resp_tx: oneshot::Sender<Result<ResponseGuard<Rx, C>>>,
}

pub struct Client<C, Tx, Rx>
where
    C: MessageCodec,
    Tx: OwnedBuf + HasAllocator,
    Rx: ReleasableBuf,
{
    workers: Arc<Vec<flume::Sender<CallRequest<C, Rx>>>>,
    rr_idx: Arc<AtomicUsize>,
    _phantom: PhantomData<(C, Tx)>,
}

impl<C, Tx, Rx> Clone for Client<C, Tx, Rx>
where
    C: MessageCodec,
    Tx: OwnedBuf + HasAllocator,
    Rx: ReleasableBuf,
{
    fn clone(&self) -> Self {
        Self {
            workers: self.workers.clone(),
            rr_idx: self.rr_idx.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<C, Tx, Rx> Client<C, Tx, Rx>
where
    C: MessageCodec<Dest = <TxAlloc<Tx> as BufferAllocator>::Payload>,
    Tx: OwnedBuf + HasAllocator,
    Rx: ReleasableBuf,
{
    #[instrument(
        level = "info",
        skip(transport, topology),
        fields(
            transport = %transport.kind(),
            num_cores = topology.map.len(),
            codec     = %C::kind(),
        )
    )]
    pub async fn connect<T>(config: ClientConfig, transport: T, topology: Topology) -> Result<Self>
    where
        T: TransportBuilder<Tx, Rx = Rx>,
    {
        if topology.transport_kind != transport.kind() {
            return Err(anyhow!(
                "Transport mismatch: registry='{}' client='{}'",
                topology.transport_kind,
                transport.kind()
            ));
        }
        if topology.codec_kind != C::kind() {
            return Err(anyhow!(
                "Codec mismatch: registry='{}' client='{}'",
                topology.codec_kind,
                C::kind()
            ));
        }

        let num_cores = topology.map.len();
        let mut connectors = Vec::with_capacity(num_cores);
        for i in 0..num_cores {
            let endpoint = topology
                .map
                .get(&i)
                .ok_or_else(|| anyhow!("Core {} not found in topology", i))?;
            connectors.push(transport.client_connector(endpoint.clone(), i)?);
        }

        Self::connect_internal(connectors, num_cores, config).await
    }

    async fn connect_internal<Conn, Si, So>(
        connectors: Vec<Conn>,
        num_cores: usize,
        config: ClientConfig,
    ) -> Result<Self>
    where
        Si: MessageSink<Payload = Tx>,
        So: MessageSource<Payload = Rx>,
        Conn: TransportConnector<Si, So>,
    {
        runtime::init(num_cores);

        let mut workers = Vec::with_capacity(num_cores);
        let (init_tx, init_rx) = flume::bounded::<Result<usize>>(num_cores);

        for (core_id, connector) in connectors.into_iter().enumerate() {
            let (worker_tx, worker_rx) = flume::unbounded::<CallRequest<C, Rx>>();
            workers.push(worker_tx);

            let sync_tx = init_tx.clone();
            let cfg = config.clone();

            runtime::spawn_on(core_id, move || async move {
                let tx_alloc: TxAlloc<Tx> = TxAlloc::<Tx>::get(&PoolConfig::default());

                let connect_res = async {
                    let connect_future = async {
                        let transport = connector.connect().await?;
                        ClientPerCore::<C, Si, So, TxAlloc<Tx>, Rx>::connect(
                            transport,
                            cfg.clone(),
                            tx_alloc,
                        )
                        .await
                    };

                    timeout(Duration::from_secs(cfg.timeout_seconds), connect_future)
                        .await
                        .map_err(|_| {
                            anyhow!("Connection timeout after {} seconds", cfg.timeout_seconds)
                        })?
                }
                .await;

                match connect_res {
                    Ok(mut client) => {
                        let _ = sync_tx.send_async(Ok(core_id)).await;
                        debug!(core_id, "Worker connected");

                        while let Ok(msg) = worker_rx.recv_async().await {
                            let res = client.call(msg.req).await;
                            if msg.resp_tx.send(res).is_err() {
                                warn!(core_id, "Caller dropped response channel");
                            }
                        }
                        info!(core_id, "Worker shut down");
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
                Ok(Ok(id)) => debug!(core_id = id, "Worker synced"),
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(anyhow!("Init channel closed prematurely")),
            }
        }

        info!(num_workers = workers.len(), "Client pool ready");

        Ok(Self {
            workers: Arc::new(workers),
            rr_idx: Arc::new(AtomicUsize::new(0)),
            _phantom: PhantomData,
        })
    }

    pub async fn call(&self, req: C::Request) -> Result<ResponseGuard<Rx, C>> {
        let idx = self.rr_idx.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        let (resp_tx, resp_rx) = oneshot::channel();

        self.workers[idx]
            .send_async(CallRequest { req, resp_tx })
            .await
            .map_err(|_| anyhow!("Worker task dropped"))?;

        resp_rx
            .await
            .map_err(|_| anyhow!("Worker response cancelled"))?
    }
}
