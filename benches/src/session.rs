use std::cell::RefCell;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::rc::Rc;

#[path = "common.rs"]
mod common;
use common::{PingRequest, PongResponse};

use ogurpchik::codecs::base::{BufferAllocator, Envelope, MessageCodec, OwnedBuf, ReleasableBuf};
use ogurpchik::codecs::rkyv_protocol::{AlignedBuffer, RkyvCodec};
use ogurpchik::high::service_handler::ServiceHandler;
use ogurpchik::low::main_loop::run_session;
use ogurpchik::pool::allocator::{SharedAllocator, TpcAllocator};
use ogurpchik::transport::base::pool_config::PoolConfig;
use dashmap::DashMap;
use local_sync::mpsc;
use rustc_hash::FxHashMap;
use local_sync::oneshot;

type Codec = RkyvCodec<PingRequest, PongResponse>;
type TxA   = TpcAllocator<AlignedBuffer>;
type RxBuf = AlignedBuffer;

#[derive(Clone)]
struct EchoHandler;

impl ServiceHandler<Codec> for EchoHandler {
    async fn on_request<'a>(&self, _req: &rkyv::Archived<PingRequest>) -> anyhow::Result<PongResponse> {
        Ok(PongResponse::Pong)
    }
}

struct InMemSink {
    tx: mpsc::bounded::Tx<AlignedBuffer>,
}

impl Clone for InMemSink {
    fn clone(&self) -> Self { Self { tx: self.tx.clone() } }
}

impl ogurpchik::transport::base::MessageSink for InMemSink {
    type Payload = AlignedBuffer;
    async fn send(&self, data: AlignedBuffer) -> anyhow::Result<()> {
        self.tx.send(data).await.map_err(|_| anyhow::anyhow!("sink closed"))
    }
}

struct InMemSource {
    rx: mpsc::bounded::Rx<AlignedBuffer>,
}

impl ogurpchik::transport::base::MessageSource for InMemSource {
    type Payload = AlignedBuffer;
    async fn recv(&mut self) -> Option<AlignedBuffer> {
        self.rx.recv().await
    }
}

fn bench_session_loop(c: &mut Criterion) {
    let mut group = c.benchmark_group("session_loop");
    const N: usize = 1_000;
    group.throughput(Throughput::Elements(N as u64));

    let alloc: TxA = BufferAllocator::get(&PoolConfig::default());

    group.bench_function("in_memory_echo", |b| {
        let runtime = compio::runtime::Runtime::new().unwrap();
        b.iter_custom(|iters| {
            runtime.block_on(async {

                let (req_tx, req_rx) = mpsc::bounded::channel::<AlignedBuffer>(256);

                let (res_tx, mut res_rx) = mpsc::bounded::channel::<AlignedBuffer>(256);

                let sink   = InMemSink   { tx: res_tx };
                let source = InMemSource { rx: req_rx };
                let pending: Rc<RefCell<FxHashMap<u64, oneshot::Sender<AlignedBuffer>>>> = Rc::new(RefCell::new(FxHashMap::default()));

                let a = alloc.clone();
                compio::runtime::spawn(async move {
                    run_session::<(Codec, TxA, RxBuf), _, _, _>(
                        EchoHandler, sink, source, pending, a,
                    ).await;
                }).detach();

                let mut req_buf = alloc.allocate_hinted();
                Codec::encode(Envelope::Request { id: 0, payload: PingRequest::Ping }, &mut req_buf).unwrap();

                let start = std::time::Instant::now();
                for i in 0..iters {
                    req_tx.send(req_buf.clone()).await.unwrap();
                    let raw = res_rx.recv().await.unwrap();
                    std::hint::black_box(raw);
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

criterion_group!(session_benches, bench_session_loop);
criterion_main!(session_benches);