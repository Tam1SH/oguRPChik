use crate::codecs::base::{
    BufferAllocator, HasAllocator, MessageCodec, OwnedBuf, ReleasableBuf,
};
use crate::discovery::kv::{KvStore, default_kv};
use crate::discovery::{RpcTopologyRegistry, ServiceRegistration, Topology};
use crate::high::client::Client;
use crate::high::server::setup;
use crate::high::service_handler::ServiceHandler;
use crate::low::client_per_core::ClientConfig;
use crate::transport::base::TransportBuilder;
use std::marker::PhantomData;
use std::sync::Arc;
use tracing::{debug, info, instrument};
use crate::discovery::Scope;

/// High-level builder for RPC nodes.
///
/// A `Node` can act as a server, a client, or both simultaneously.
/// Service discovery is handled automatically via the Windows registry.
///
/// ## Thread configuration
///
/// Single-threaded by default. Use `.threads(n)` to pin server workers to cores.
///
/// # Examples
///
/// ## Loopback (server + client in the same process)
///
/// ```rust,no_run
/// # use ogurpchik::high::node::Node;
/// # use ogurpchik::transport::stream::adapters::tcp::TcpTransport;
/// # use ogurpchik::codecs::rkyv_protocol::RkyvCodec;
/// # use ogurpchik::high::service_handler::ServiceHandler;
/// # use rkyv::{Archive, Serialize, Deserialize};
/// # #[derive(Archive, Serialize, Deserialize)] enum Req { Ping }
/// # #[derive(Archive, Serialize, Deserialize)] enum Res { Pong }
/// # type MyCodec = RkyvCodec<Req, Res>;
/// # #[derive(Clone)] struct MyHandler;
/// # impl ServiceHandler<MyCodec> for MyHandler {
/// #     async fn on_request<'a>(&self, _: &ArchivedReq) -> anyhow::Result<Res> { Ok(Res::Pong) }
/// # }
/// # async fn example() -> anyhow::Result<()> {
/// let (client, _guard) = Node::new()?
///     .serve::<MyCodec, _, _>(TcpTransport::new("127.0.0.1"), MyHandler)
///     .connect::<MyCodec, _>(TcpTransport::new("127.0.0.1"))
///     .start()
///     .await?;
///
/// let _response = client.call(Req::Ping).await?;
/// # Ok(())
/// # }
/// ```
///
/// ## Multi-threaded server
///
/// ```rust,no_run
/// # async fn example() -> anyhow::Result<()> {
/// let (client, _guard) = Node::new()?
///     .threads(4)
///     .serve::<MyCodec, _, _>(TcpTransport::new("127.0.0.1"), MyHandler)
///     .connect::<MyCodec, _>(TcpTransport::new("127.0.0.1"))
///     .start()
///     .await?;
/// # Ok(())
/// # }
/// ```
///
/// ## Serve only
///
/// ```rust,no_run
/// # async fn example() -> anyhow::Result<()> {
/// let _guard = Node::new()?
///     .serve::<MyCodec, _, _>(TcpTransport::new("127.0.0.1"), MyHandler)
///     .publish("my-service")
///     .start()
///     .await?;
/// # Ok(())
/// # }
/// ```
///
/// ## Connect only
///
/// ```rust,no_run
/// # async fn example() -> anyhow::Result<()> {
/// let client = Node::new()?
///     .connect::<MyCodec, _>(TcpTransport::new("127.0.0.1"))
///     .wait_for("my-service")
///     .start()
///     .await?;
/// # Ok(())
/// # }
/// ```
pub struct Node<S, C> {
    serve: S,
    connect: C,
    kv: Arc<dyn KvStore>,
    threads: usize,
}

pub struct NoServe;
pub struct NoConnect;
pub struct NoPayload;

pub struct ServeConfig<H, T, Codec> {
    transport: T,
    handler: H,
    publish_as: Option<String>,
    _codec: PhantomData<Codec>,
}
pub struct ConnectConfig<T, Codec> {
    transport: T,
    wait_for: Option<String>,
    config: Option<ClientConfig>,
    _codec: PhantomData<Codec>,
}

impl Node<NoServe, NoConnect> {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            serve: NoServe,
            connect: NoConnect,
            kv: Arc::new(default_kv()?),
            threads: 1,
        })
    }

    pub fn scope(mut self, scope: Scope) -> anyhow::Result<Self> {
        self.kv = scope.into_kv()?;
        Ok(self)
    }

    pub fn kv(mut self, kv: Arc<dyn KvStore>) -> Self {
        self.kv = kv;
        self
    }
}

impl<S, C> Node<S, C> {
    pub fn single_thread(mut self) -> Self {
        self.threads = 1;
        self
    }

    pub fn threads(mut self, n: usize) -> Self {
        self.threads = n;
        self
    }
}

impl<C> Node<NoServe, C> {
    pub fn serve<Codec, H, T>(self, transport: T, handler: H) -> Node<ServeConfig<H, T, Codec>, C>
    where
        Codec: MessageCodec,
        T: TransportBuilder<Codec::Dest>,
        H: ServiceHandler<Codec>,
    {
        Node {
            serve: ServeConfig {
                transport,
                handler,
                publish_as: None,
                _codec: PhantomData,
            },
            connect: self.connect,
            kv: self.kv,
            threads: self.threads,
        }
    }
}

impl<S> Node<S, NoConnect> {
    pub fn connect<Codec, T>(self, transport: T) -> Node<S, ConnectConfig<T, Codec>>
    where
        Codec: MessageCodec,
        T: TransportBuilder<Codec::Dest>,
    {
        Node {
            serve: self.serve,
            connect: ConnectConfig {
                transport,
                wait_for: None,
                config: None,
                _codec: PhantomData,
            },
            kv: self.kv,
            threads: self.threads,
        }
    }
}

impl<H, T, Codec, C> Node<ServeConfig<H, T, Codec>, C> {
    pub fn publish(mut self, name: impl Into<String>) -> Self {
        self.serve.publish_as = Some(name.into());
        self
    }
}

impl<S, T, Codec> Node<S, ConnectConfig<T, Codec>> {
    pub fn wait_for(mut self, name: impl Into<String>) -> Self {
        self.connect.wait_for = Some(name.into());
        self
    }

    pub fn timeout(mut self, secs: u64) -> Self {
        let mut cfg = self.connect.config.unwrap_or_default();
        cfg.timeout_seconds = secs;
        self.connect.config = Some(cfg);
        self
    }
}

impl<H, ST, SC, SCodec, CCodec> Node<ServeConfig<H, ST, SCodec>, ConnectConfig<SC, CCodec>>
where
    SCodec: MessageCodec,
    CCodec: MessageCodec<Dest = SCodec::Dest>,
    SCodec::Dest: OwnedBuf + HasAllocator,
    ST: TransportBuilder<SCodec::Dest>,
    SC: TransportBuilder<SCodec::Dest>,
    ST::Rx: ReleasableBuf,
    SC::Rx: ReleasableBuf,
    H: ServiceHandler<SCodec>,
{
    #[instrument(skip(self), fields(
        serve_transport = %self.serve.transport.kind(),
        serve_codec     = %SCodec::kind(),
        connect_codec   = %CCodec::kind(),
        publish_as      = ?self.serve.publish_as,
        wait_for        = ?self.connect.wait_for,
    ))]
    pub async fn start(
        self,
    ) -> anyhow::Result<(
        Client<CCodec, CCodec::Dest, SC::Rx>,
        Option<ServiceRegistration>,
    )> {
        info!("Node starting");

        let (local_topology, guard) = start_server::<H, ST, SCodec, CCodec::Dest>(
            self.serve.transport,
            self.serve.handler,
            self.serve.publish_as,
            self.kv.clone(),
            self.threads,
        )
        .await?;

        let topology =
            resolve_remote(self.connect.wait_for.as_deref(), local_topology, &self.kv).await?;

        info!("Connecting client");
        let client = Client::<CCodec, CCodec::Dest, SC::Rx>::connect(
            self.connect.config.unwrap_or_default(),
            self.connect.transport,
            topology,
        )
        .await?;

        info!("Node fully started");
        Ok((client, guard))
    }
}

// ── serve only ────────────────────────────────────────────────────────────────

impl<H, T, Codec> Node<ServeConfig<H, T, Codec>, NoConnect>
where
    Codec: MessageCodec,
    Codec::Dest: OwnedBuf + HasAllocator,
    T: TransportBuilder<Codec::Dest>,
    T::Rx: ReleasableBuf,
    H: ServiceHandler<Codec>,
{
    #[instrument(skip(self), fields(
        transport  = %self.serve.transport.kind(),
        codec      = %Codec::kind(),
        publish_as = ?self.serve.publish_as,
    ))]
    pub async fn start(self) -> anyhow::Result<Option<ServiceRegistration>> {
        info!("Node starting (serve only)");
        let (_, guard) = start_server::<H, T, Codec, Codec::Dest>(
            self.serve.transport,
            self.serve.handler,
            self.serve.publish_as,
            self.kv,
            self.threads,
        )
        .await?;
        Ok(guard)
    }
}

// ── connect only ──────────────────────────────────────────────────────────────

impl<T, Codec> Node<NoServe, ConnectConfig<T, Codec>>
where
    Codec: MessageCodec,
    Codec::Dest: OwnedBuf + HasAllocator,
    T: TransportBuilder<Codec::Dest>,
    T::Rx: ReleasableBuf,
{
    #[instrument(skip(self), fields(
        transport = %self.connect.transport.kind(),
        codec     = %Codec::kind(),
        wait_for  = ?self.connect.wait_for,
    ))]
    pub async fn start(self) -> anyhow::Result<Client<Codec, Codec::Dest, T::Rx>> {
        info!("Node starting (connect only)");

        let topology = resolve_remote(
            self.connect.wait_for.as_deref(),
            Topology::default(),
            &self.kv,
        )
        .await?;

        let client = Client::<Codec, Codec::Dest, T::Rx>::connect(
            self.connect.config.unwrap_or_default(),
            self.connect.transport,
            topology,
        )
        .await?;

        info!("Node connected");
        Ok(client)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

type TxAlloc<P> = <P as HasAllocator>::Alloc;

async fn start_server<H, T, Codec, P>(
    transport: T,
    handler: H,
    publish_as: Option<String>,
    kv: Arc<dyn KvStore>,
    num_threads: usize,
) -> anyhow::Result<(Topology, Option<ServiceRegistration>)>
where
    P: OwnedBuf + HasAllocator,
    Codec: MessageCodec<Dest = <TxAlloc<P> as BufferAllocator>::Payload>,
    T: TransportBuilder<P>,
    T::Rx: ReleasableBuf,
    H: ServiceHandler<Codec>,
{
    let registry = RpcTopologyRegistry::builder(transport.kind(), Codec::kind())
        .publish_as(publish_as.unwrap_or_default())
        .kv(kv)
        .build()?;

    let srv_reg = registry.clone();
    compio::runtime::spawn(async move {
        setup()
            .with_transport(transport)
            .with_registry(srv_reg)
            .threads(num_threads)
            .service::<_, Codec>(handler)
            .run()
            .await
            .expect("Server error");
    })
    .detach();

    info!("Waiting for server to become ready");
    let result = registry.ready().await;
    info!("Server ready");
    Ok(result)
}

async fn resolve_remote(
    wait_for: Option<&str>,
    local_topology: Topology,
    kv: &Arc<dyn KvStore>,
) -> anyhow::Result<Topology> {
    match wait_for {
        Some(name) => {
            info!(service = %name, "Waiting for remote service");
            Topology::watch(name, kv.clone()).await?;
            info!(service = %name, "Remote service appeared");
            let t = Topology::resolve(name, kv.as_ref())?;
            debug!(service = %name, topology = ?t, "Topology resolved");
            Ok(t)
        }
        None => {
            debug!("No wait_for, using local server topology");
            Ok(local_topology)
        }
    }
}
