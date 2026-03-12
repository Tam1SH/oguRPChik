#![allow(dead_code)]

use std::path::PathBuf;
use compio::net::{UnixListener, UnixStream};
use ogurpchik::{
    codecs::{
        base::{BufferAllocator, HasAllocator, MessageCodec, OwnedBuf, ReleasableBuf},
        rkyv_protocol::RkyvCodec,
        serde_compatible::bitcode::BitcodeCodec,
        serde_protocol::VecBuf,
    },
    discovery::ServiceRegistration,
    high::{
        client::Client,
        node::Node,
        service_handler::ServiceHandler,
    },
    pool::{
        allocator::{SharedAllocator, TpcAllocator},
        buf_guard::BufGuard,
    },
    transport::{
        base::{MessageSink, TransportBuilder, pool_config::PoolConfig},
        impls::peer::{implementation::Peer, config::PeerConfig, handle::{PeerSink, PeerSource}},
        stream::vsock::{VsockTarget, general::{VListener, VStream}},
    },
};
use rkyv::{Archive, Deserialize, Serialize};
use ogurpchik::codecs::rkyv_protocol::AlignedBuffer;
use ogurpchik::transport::base::MessageSource;
use ogurpchik::transport::stream::AsyncStream;

#[derive(
    Archive, Deserialize, Serialize, Debug, PartialEq, Clone,
    serde::Deserialize, serde::Serialize,
)]
#[rkyv(compare(PartialEq), derive(Debug, PartialEq, Eq))]
pub enum PingRequest { Ping }

#[derive(
    Archive, Deserialize, Serialize, Debug, PartialEq, Clone,
    serde::Deserialize, serde::Serialize,
)]
#[rkyv(compare(PartialEq), derive(Debug, PartialEq, Eq))]
pub enum PongResponse { Pong }

#[derive(Clone)]
pub struct EchoHandler;

impl ServiceHandler<RkyvCodec<PingRequest, PongResponse>> for EchoHandler {
    async fn on_request<'a>(&self, req: &ArchivedPingRequest) -> anyhow::Result<PongResponse> {
        match req {
            ArchivedPingRequest::Ping => Ok(PongResponse::Pong),
        }
    }
}

impl ServiceHandler<BitcodeCodec<PingRequest, PongResponse>> for EchoHandler {
    async fn on_request<'a>(&self, req: PingRequest) -> anyhow::Result<PongResponse> {
        Ok(PongResponse::Pong)
    }
}

pub type RkyvClient = Client<
    RkyvCodec<PingRequest, PongResponse>,
    AlignedBuffer,
    BufGuard<AlignedBuffer, SharedAllocator<AlignedBuffer>>,
>;

pub type BitcodeClient<T> = Client<
    BitcodeCodec<PingRequest, PongResponse>,
    <BitcodeCodec<PingRequest, PongResponse> as MessageCodec>::Dest,
    <T as TransportBuilder<
        <BitcodeCodec<PingRequest, PongResponse> as MessageCodec>::Dest
    >>::Rx,
>;


pub fn run_block<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    compio::runtime::Runtime::new()
        .expect("failed to create compio runtime")
        .block_on(f)
}

type RkyvDest = AlignedBuffer;

type BitcodeDest = VecBuf;

pub async fn make_rkyv_node<T>(
    transport: T,
) -> (RkyvClient, Option<ServiceRegistration>)
where
    T: TransportBuilder<RkyvDest, Rx = BufGuard<RkyvDest, SharedAllocator<RkyvDest>>> + Clone + 'static,
    <T as TransportBuilder<AlignedBuffer>>::Rx: ReleasableBuf,
    RkyvDest: OwnedBuf + HasAllocator,
{
    Node::new()
        .expect("Node::new failed")
        .serve::<RkyvCodec<PingRequest, PongResponse>, _, _>(
            transport.clone(),
            EchoHandler,
        )
        .connect::<RkyvCodec<PingRequest, PongResponse>, _>(transport)
        .start()
        .await
        .expect("Node start failed")
}

pub async fn make_bitcode_node<T>(
    transport: T,
) -> (BitcodeClient<T>, Option<ServiceRegistration>)
where
    T: TransportBuilder<BitcodeDest> + Clone + 'static,
    T::Rx: ReleasableBuf,
    BitcodeDest: OwnedBuf + HasAllocator,
{
    Node::new()
        .expect("Node::new failed")
        .serve::<BitcodeCodec<PingRequest, PongResponse>, _, _>(
            transport.clone(),
            EchoHandler,
        )
        .connect::<BitcodeCodec<PingRequest, PongResponse>, _>(transport)
        .start()
        .await
        .expect("Node start failed")
}


pub type PeerTx = PeerSink<VecBuf>;
pub type PeerRx = PeerSource<BufGuard<VecBuf, SharedAllocator<VecBuf>>>;

pub async fn make_vsock_peer_pair(port: u32) -> (PeerTx, PeerRx) {
    let (srv, cli) = vsock_pair(port).await;
    let (tx, _) = make_peer(srv);
    let (_, rx) = make_peer(cli);
    (tx, rx)
}

pub async fn make_vsock_peer_pair_full(port: u32) -> ((PeerTx, PeerRx), (PeerTx, PeerRx), u32) {
    let (srv, cli) = vsock_pair(port).await;
    let (s_tx, s_rx) = make_peer(srv);
    let (c_tx, c_rx) = make_peer(cli);
    ((s_tx, s_rx), (c_tx, c_rx), port)
}

pub async fn send_recv_n(mut tx: PeerTx, rx: &mut PeerRx, n: usize, msg_size: usize) {
    let sender = compio::runtime::spawn(async move {
        for _ in 0..n {
            tx.send(vec![0xABu8; msg_size].into()).await.unwrap();
        }
    });
    for _ in 0..n {
        std::hint::black_box(rx.recv().await.unwrap());
    }
    sender.await.unwrap();
}

async fn vsock_pair(port: u32) -> (VStream, VStream) {
    let listener = VListener::bind_loopback(port).expect("vsock bind failed");
    let h = compio::runtime::spawn(async move {
        listener.accept().await.expect("vsock accept failed")
    });
    let cli = VStream::connect(VsockTarget::Cid(0), port)
        .await
        .expect("vsock connect failed");
    let (srv, _) = h.await.unwrap();
    (srv, cli)
}

pub async fn make_tcp_peer_pair() -> (PeerTx, PeerRx) {
    use compio::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("tcp bind failed");
    let addr = listener.local_addr().expect("local_addr failed");
    let h = compio::runtime::spawn(async move {
        listener.accept().await.expect("tcp accept failed")
    });
    let cli = TcpStream::connect(addr).await.expect("tcp connect failed");
    let (srv, _) = h.await.unwrap();
    let (tx, _) = make_peer(srv);
    let (_, rx) = make_peer(cli);
    (tx, rx)
}

pub type AlignedPeerTx = PeerSink<AlignedBuffer>;
pub type AlignedPeerRx = PeerSource<BufGuard<AlignedBuffer, SharedAllocator<AlignedBuffer>>>;

pub async fn make_uds_aligned_full(path: &PathBuf)
                                   -> (AlignedPeerTx, AlignedPeerRx, AlignedPeerTx, AlignedPeerRx)
{
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).await.unwrap();
    let h = compio::runtime::spawn(async move {
        listener.accept().await.unwrap()
    });
    let cli = UnixStream::connect(path).await.unwrap();
    let (srv, _) = h.await.unwrap();

    let tx: TpcAllocator<AlignedBuffer>    = BufferAllocator::get(&PoolConfig::default());
    let rx: SharedAllocator<AlignedBuffer> = BufferAllocator::get(&PoolConfig::default());

    let (srv_tx, srv_rx) = Peer::new(srv, PeerConfig::default(), tx.clone(), rx.clone()).unwrap();
    let (cli_tx, cli_rx) = Peer::new(cli, PeerConfig::default(), tx, rx).unwrap();
    (srv_tx, srv_rx, cli_tx, cli_rx)
}


pub async fn make_uds_peer_pair(path: &std::path::PathBuf) -> (PeerTx, PeerRx) {
    use compio::net::{UnixListener, UnixStream};
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).await.expect("uds bind failed");
    let path2 = path.clone();
    let h = compio::runtime::spawn(async move {
        listener.accept().await.expect("uds accept failed")
    });
    let cli = UnixStream::connect(&path2).await.expect("uds connect failed");
    let (srv, _) = h.await.unwrap();
    let (tx, _) = make_peer(srv);
    let (_, rx) = make_peer(cli);
    (tx, rx)
}

fn make_peer(
    stream: impl AsyncStream + Clone + 'static,
) -> (PeerTx, PeerRx) {
    let tx: TpcAllocator<VecBuf>    = BufferAllocator::get(&PoolConfig::default());
    let rx: SharedAllocator<VecBuf> = BufferAllocator::get(&PoolConfig::default());
    Peer::new(stream, PeerConfig::default(), tx, rx).unwrap()
}