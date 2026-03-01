use std::marker::PhantomData;
use std::sync::Arc;
use tracing::{debug, info, instrument};
use crate::client::Client;
use crate::codecs::base::MessageCodec;
use crate::discovery::kv::{default_kv, KvStore};
use crate::discovery::{RpcTopologyRegistry, ServiceRegistration, Topology};
use crate::server::{setup, HasDefaultAllocator};
use crate::service_handler::ServiceHandler;
use crate::transport::base::TransportBuilder;

/// High-level builder for RPC nodes.
///
/// A `Node` can act as a server, a client, or both simultaneously.
/// Service discovery is handled automatically via the Windows registry.
///
/// # Examples
///
/// ## Loopback (server + client in the same process)
///
/// ```rust,no_run
/// # use ogurpchik::node::Node;
/// # use ogurpchik::transport::stream::adapters::tcp::TcpTransport;
/// # use ogurpchik::codecs::rkyv_protocol::RkyvCodec;
/// # use ogurpchik::service_handler::ServiceHandler;
/// # use rkyv::{Archive, Serialize, Deserialize};
/// # #[derive(Archive, Serialize, Deserialize)] enum Req { Ping }
/// # #[derive(Archive, Serialize, Deserialize)] enum Res { Pong }
/// # type MyCodec = RkyvCodec<Req, Res>;
/// # #[derive(Clone)] struct MyHandler;
/// # impl ServiceHandler<MyCodec> for MyHandler {
/// #     async fn on_request<'a>(&self, _: &ArchivedReq) -> anyhow::Result<Res> { Ok(Res::Pong) }
/// # }
/// # async fn example() -> anyhow::Result<()> {
/// let (client, _guard) = Node::new()
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
/// ## Serve only
///
/// ```rust,no_run
/// # use ogurpchik::node::Node;
/// # use ogurpchik::transport::stream::adapters::tcp::TcpTransport;
/// # async fn example() -> anyhow::Result<()> {
/// let _guard = Node::new()
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
/// # use ogurpchik::node::Node;
/// # use ogurpchik::transport::stream::adapters::tcp::TcpTransport;
/// # async fn example() -> anyhow::Result<()> {
/// let client = Node::new()
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
}

pub struct NoServe;
pub struct NoConnect;

pub struct ServeConfig<H, T, Codec> {
    transport: T,
    handler: H,
    publish_as: Option<String>,
    _codec: PhantomData<Codec>,
}

pub struct ConnectConfig<T, Codec> {
    transport: T,
    wait_for: Option<String>,
    _codec: PhantomData<Codec>,
}

impl Node<NoServe, NoConnect> {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            serve: NoServe,
            connect: NoConnect,
            kv: Arc::new(default_kv()?),
        })
    }

    pub fn kv(mut self, kv: Arc<dyn KvStore>) -> Self {
        self.kv = kv;
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
                _codec: PhantomData,
            },
            kv: self.kv,
        }
    }
}

impl<H, T, Codec, C> Node<ServeConfig<H, T, Codec>, C> {
    pub fn publish(mut self, service_name: impl Into<String>) -> Self {
        self.serve.publish_as = Some(service_name.into());
        self
    }
}

impl<S, T, Codec> Node<S, ConnectConfig<T, Codec>> {
    pub fn wait_for(mut self, service_name: impl Into<String>) -> Self {
        self.connect.wait_for = Some(service_name.into());
        self
    }
}


impl<H, ST, SC, SCodec, CCodec, P> Node<ServeConfig<H, ST, SCodec>, ConnectConfig<SC, CCodec>>
where
    SCodec: MessageCodec<Dest = P>,
    CCodec: MessageCodec<Dest = P>,
    ST: TransportBuilder<SCodec::Dest> + Clone + Send + Sync + 'static,
    SC: TransportBuilder<P> + Send + Sync + 'static,
    H: ServiceHandler<SCodec> + Clone,
    P: AsRef<[u8]> + Send + HasDefaultAllocator + 'static,
{
    #[instrument(skip(self), fields(
        serve_transport = %self.serve.transport.kind(),
        serve_codec = %SCodec::kind(),
        connect_codec = %CCodec::kind(),
        publish_as = ?self.serve.publish_as,
        wait_for = ?self.connect.wait_for,
    ))]
    pub async fn start(self) -> anyhow::Result<(Client<CCodec, P>, Option<ServiceRegistration>)> {
        info!("Node starting");
        let (local_topology, guard) = start_server::<_, _, SCodec>(
            self.serve.transport,
            self.serve.handler,
            self.serve.publish_as,
            self.kv.clone(),
        ).await?;

        let topology = resolve_remote(
            self.connect.wait_for.as_deref(),
            local_topology,
            &self.kv,
        ).await?;

        info!("Connecting client");
        let client = Client::<CCodec, P>::connect(self.connect.transport, topology).await?;
        info!("Node fully started");
        Ok((client, guard))
    }
}

impl<H, T, Codec> Node<ServeConfig<H, T, Codec>, NoConnect>
where
    Codec: MessageCodec,
    T: TransportBuilder<Codec::Dest> + Clone + Send + Sync + 'static,
    H: ServiceHandler<Codec> + Clone,
    <Codec as MessageCodec>::Dest: HasDefaultAllocator,
{
    #[instrument(skip(self), fields(
        transport = %self.serve.transport.kind(),
        codec = %Codec::kind(),
        publish_as = ?self.serve.publish_as,
    ))]
    pub async fn start(self) -> anyhow::Result<Option<ServiceRegistration>> {
        info!("Node starting (serve only)");
        let (_, guard) = start_server::<_, _, Codec>(
            self.serve.transport,
            self.serve.handler,
            self.serve.publish_as,
            self.kv,
        ).await?;
        Ok(guard)
    }
}


impl<T, Codec, P> Node<NoServe, ConnectConfig<T, Codec>>
where
    Codec: MessageCodec<Dest = P>,
    T: TransportBuilder<P> + Clone,
    P: AsRef<[u8]> + Send + HasDefaultAllocator + 'static,
{
    #[instrument(skip(self), fields(
        transport = %self.connect.transport.kind(),
        codec = %Codec::kind(),
        wait_for = ?self.connect.wait_for,
    ))]
    pub async fn start(self) -> anyhow::Result<Client<Codec, P>> {
        info!("Node starting (connect only)");
        let topology = resolve_remote(
            self.connect.wait_for.as_deref(),
            Topology::default(),
            &self.kv,
        ).await?;

        let client = Client::<Codec, P>::connect(self.connect.transport, topology).await?;
        info!("Node connected");
        Ok(client)
    }

}

async fn start_server<H, T, Codec>(
    transport: T,
    handler: H,
    publish_as: Option<String>,
    kv: Arc<dyn KvStore>,
) -> anyhow::Result<(Topology, Option<ServiceRegistration>)>
where
    Codec: MessageCodec,
    T: TransportBuilder<Codec::Dest> + Clone + Send + Sync + 'static,
    H: ServiceHandler<Codec> + Clone,
    Codec::Dest: HasDefaultAllocator,
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
            .single_thread()
            .service(handler)
            .run()
            .await
            .expect("Server error");
    }).detach();

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
            let rx = Topology::watch(name, kv.clone())?;
            rx.recv_async().await?;
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
