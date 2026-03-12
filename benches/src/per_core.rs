use std::cell::RefCell;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::time::Duration;

#[path = "common.rs"]
mod common;
use common::{PingRequest, PongResponse, make_uds_peer_pair};

use ogurpchik::codecs::base::BufferAllocator;
use ogurpchik::codecs::rkyv_protocol::{AlignedBuffer, RkyvCodec};
use ogurpchik::low::client_per_core::{ClientConfig, ClientPerCore};
use ogurpchik::low::main_loop::{run_session, SessionConfig};
use ogurpchik::pool::allocator::{SharedAllocator, TpcAllocator};
use ogurpchik::transport::base::{MessageSink, MessageSource, Transport};
use ogurpchik::transport::base::pool_config::PoolConfig;
use dashmap::DashMap;
use std::rc::Rc;
use rustc_hash::FxHashMap;
use ogurpchik::codecs::serde_protocol::VecBuf;
use ogurpchik::pool::buf_guard::BufGuard;
use crate::common::{make_uds_aligned_full};

type Codec = RkyvCodec<PingRequest, PongResponse>;
type TxA   = TpcAllocator<AlignedBuffer>;
type RxBuf = BufGuard<
    AlignedBuffer,
    SharedAllocator<AlignedBuffer>,
>;

#[derive(Clone)]
struct EchoHandler;

impl ogurpchik::high::service_handler::ServiceHandler<Codec> for EchoHandler {
    async fn on_request<'a>(&self, _req: &rkyv::Archived<PingRequest>) -> anyhow::Result<PongResponse> {
        Ok(PongResponse::Pong)
    }
}

struct DirectTransport<Si, So>(Si, So);

impl<Si: MessageSink + 'static, So: MessageSource + 'static> Transport<Si, So> for DirectTransport<Si, So> {
    fn decompose(self) -> anyhow::Result<(Si, So)> {
        Ok((self.0, self.1))
    }
}

fn bench_per_core_call(c: &mut Criterion) {
    let mut group = c.benchmark_group("per_core_call");
    group.sampling_mode(criterion::SamplingMode::Flat);
    group.sample_size(200);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(2));

    let alloc: TxA = BufferAllocator::get(&PoolConfig::default());
    let uds_path = std::env::temp_dir()
        .join(format!("per_core_bench_{}.sock", std::process::id()));

    group.bench_function("uds_rkyv", |b| {
        let runtime = compio::runtime::Runtime::new().unwrap();

        let mut client = runtime.block_on(async {
            let (srv_tx, mut srv_rx, cli_tx, mut cli_rx) =
                make_uds_aligned_full(&uds_path).await;

            let a = alloc.clone();
            compio::runtime::spawn(async move {
                let pending = Rc::new(RefCell::new(FxHashMap::default()));
                run_session::<(Codec, TxA, _), _, _, _>(
                    EchoHandler, srv_tx, srv_rx, pending, a,
                ).await;
            }).detach();

            ClientPerCore::<Codec, _, _, _, _>::connect(
                DirectTransport(cli_tx, cli_rx),
                ClientConfig::default(),
                alloc.clone(),
            ).await.unwrap()
        });

        b.iter_custom(|iters| {
            runtime.block_on(async {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    std::hint::black_box(
                        client.call(PingRequest::Ping).await.unwrap()
                    );
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

criterion_group!(per_core_benches, bench_per_core_call);
criterion_main!(per_core_benches);