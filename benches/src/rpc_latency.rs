use criterion::{BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main};
use std::time::Duration;

#[path = "common.rs"]
mod common;
use common::{PingRequest, make_rkyv_node, make_bitcode_node, run_block};

use ogurpchik::transport::stream::adapters::{
    tcp::TcpTransport,
    uds::UdsTransport,
    vsock::{VsockAddr, VsockTransport},
    shm::ShmTransport,
};

fn bench_tcp_rkyv(c: &mut Criterion) {
    let mut group = c.benchmark_group("tcp_rkyv");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(200);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(2));

    let runtime = compio::runtime::Runtime::new().unwrap();
    let (client, _) = runtime.block_on(async {
        make_rkyv_node(TcpTransport::new("127.0.0.1".to_string())).await
    });

    group.bench_function("ping_pong", |b| {
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    std::hint::black_box(client.call(PingRequest::Ping).await.unwrap());
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

fn bench_uds_rkyv(c: &mut Criterion) {
    let mut group = c.benchmark_group("uds_rkyv");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(200);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(2));

    let runtime = compio::runtime::Runtime::new().unwrap();
    let (client, _) = runtime.block_on(async {
        make_rkyv_node(UdsTransport::temp(&format!("rpc_bench_{}", std::process::id()))).await
    });

    group.bench_function("ping_pong", |b| {
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    std::hint::black_box(client.call(PingRequest::Ping).await.unwrap());
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

fn bench_vsock_rkyv(c: &mut Criterion) {
    let mut group = c.benchmark_group("vsock_rkyv");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(200);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(2));

    let runtime = compio::runtime::Runtime::new().unwrap();
    let (client, _) = runtime.block_on(async {
        make_rkyv_node(VsockTransport::server(VsockAddr::Cid(0), 5100)).await
    });

    group.bench_function("ping_pong", |b| {
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    std::hint::black_box(client.call(PingRequest::Ping).await.unwrap());
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

fn bench_shm_bitcode(c: &mut Criterion) {
    let mut group = c.benchmark_group("shm_bitcode");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(500);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(2));

    let runtime = compio::runtime::Runtime::new().unwrap();
    let (client, _) = runtime.block_on(async {
        make_bitcode_node(ShmTransport::new(&format!("bench_lat_{}", std::process::id()))).await
    });

    group.bench_function("ping_pong", |b| {
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    std::hint::black_box(client.call(PingRequest::Ping).await.unwrap());
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

fn bench_transport_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("transport_comparison");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(200);

    let runtime = compio::runtime::Runtime::new().unwrap();

    let transports: Vec<(&str, common::RkyvClient)> = runtime.block_on(async {
        let (c1, _) = make_rkyv_node(TcpTransport::new("127.0.0.1".to_string())).await;
        let (c2, _) = make_rkyv_node(UdsTransport::temp(&format!("rpc_cmp_{}", std::process::id()))).await;
        let (c3, _) = make_rkyv_node(VsockTransport::server(VsockAddr::Cid(0), 5101)).await;
        vec![("tcp", c1), ("uds", c2), ("vsock", c3)]
    });

    let (shm_client, _) = runtime.block_on(async {
        make_bitcode_node(ShmTransport::new(&format!("bench_cmp_{}", std::process::id()))).await
    });

    for (label, client) in &transports {
        group.bench_with_input(BenchmarkId::new("rkyv", label), label, |b, _| {
            b.iter_custom(|iters| {
                runtime.block_on(async {
                    let start = std::time::Instant::now();
                    for _ in 0..iters {
                        std::hint::black_box(client.call(PingRequest::Ping).await.unwrap());
                    }
                    start.elapsed()
                })
            });
        });
    }

    group.bench_function(BenchmarkId::new("bitcode", "shm"), |b| {
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    std::hint::black_box(shm_client.call(PingRequest::Ping).await.unwrap());
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

criterion_group! {
    name   = rpc_latency_benches;
    config = Criterion::default();
    targets = bench_tcp_rkyv, bench_uds_rkyv, bench_vsock_rkyv, bench_shm_bitcode,
              bench_transport_comparison
}
criterion_main!(rpc_latency_benches);