//! ```sh
//! cargo bench --bench rpc_throughput --features all
//! ```

use criterion::{
    BenchmarkId, Criterion, Throughput, SamplingMode,
    criterion_group, criterion_main,
};
use futures::future::join_all;
use std::time::Duration;

#[path = "common.rs"]
mod common;
use common::{
    PingRequest, make_rkyv_node, make_bitcode_node, run_block,
};

use ogurpchik::transport::stream::adapters::{
    tcp::TcpTransport,
    vsock::{VsockAddr, VsockTransport},
    shm::ShmTransport,
};

const SEQ_COUNTS:   &[usize] = &[1, 10, 100];
const CONCUR_DEPTH: &[usize] = &[2, 8, 32, 128];

// ─── Sequential calls ─────────────────────────────────────────────────────────

fn bench_sequential_calls(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequential_calls");
    group.sampling_mode(SamplingMode::Linear);
    group.sample_size(50);

    // Один клиент на всю группу.
    let (tcp_client, _tcp_guard) = run_block(async {
        make_rkyv_node(TcpTransport::new("127.0.0.1".to_string())).await
    });
    // let shm_svc = format!("bench_seq_{}", std::process::id());
    // let (shm_client, _shm_guard) = run_block(async {
    //     make_bitcode_node(ShmTransport::new(&shm_svc)).await
    // });

    for &n in SEQ_COUNTS {
        group.throughput(Throughput::Elements(n as u64));

        group.bench_with_input(BenchmarkId::new("tcp_rkyv", n), &n, |b, &count| {
            b.iter(|| run_block(async {
                for _ in 0..count {
                    std::hint::black_box(
                        tcp_client.call(PingRequest::Ping).await.unwrap()
                    );
                }
            }));
        });
        //
        // group.bench_with_input(BenchmarkId::new("shm_bitcode", n), &n, |b, &count| {
        //     b.iter(|| run_block(async {
        //         for _ in 0..count {
        //             std::hint::black_box(
        //                 shm_client.call(PingRequest::Ping).await.unwrap()
        //             );
        //         }
        //     }));
        // });
    }

    group.finish();
}

// ─── Concurrent calls — futures::join_all ────────────────────────────────────

fn bench_concurrent_calls(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_calls");
    group.sampling_mode(SamplingMode::Linear);
    group.sample_size(50);

    let (tcp_client, _tcp_guard) = run_block(async {
        make_rkyv_node(TcpTransport::new("127.0.0.1".to_string())).await
    });
    // let shm_svc = format!("bench_conc_{}", std::process::id());
    // let (shm_client, _shm_guard) = run_block(async {
    //     make_bitcode_node(ShmTransport::new(&shm_svc)).await
    // });

    for &depth in CONCUR_DEPTH {
        group.throughput(Throughput::Elements(depth as u64));

        group.bench_with_input(BenchmarkId::new("tcp_rkyv", depth), &depth, |b, &d| {
            b.iter(|| run_block(async {
                let futs: Vec<_> = (0..d)
                    .map(|_| tcp_client.call(PingRequest::Ping))
                    .collect();
                let results = join_all(futs).await;
                for r in results {
                    std::hint::black_box(r.unwrap());
                }
            }));
        });
        //
        // group.bench_with_input(BenchmarkId::new("shm_bitcode", depth), &depth, |b, &d| {
        //     b.iter(|| run_block(async {
        //         let futs: Vec<_> = (0..d)
        //             .map(|_| shm_client.call(PingRequest::Ping))
        //             .collect();
        //         let results = join_all(futs).await;
        //         for r in results {
        //             std::hint::black_box(r.unwrap());
        //         }
        //     }));
        // });
    }

    group.finish();
}

// ─── Bulk comparison — все транспорты при фиксированном N ────────────────────

fn bench_bulk_comparison(c: &mut Criterion) {
    const N: usize = 1_000;

    let mut group = c.benchmark_group("bulk_1000_calls");
    group.throughput(Throughput::Elements(N as u64));
    group.sampling_mode(SamplingMode::Linear);
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(15));

    let (tcp_client,   _tcp_guard)   = run_block(async {
        make_rkyv_node(TcpTransport::new("127.0.0.1".to_string())).await
    });
    let (vsock_client, _vsock_guard) = run_block(async {
        make_rkyv_node(VsockTransport::server(VsockAddr::Cid(0), 5200)).await
    });
    // let shm_svc = format!("bench_bulk_{}", std::process::id());
    // let (shm_client,   _shm_guard)   = run_block(async {
    //     make_bitcode_node(ShmTransport::new(&shm_svc)).await
    // });

    group.bench_function("tcp_rkyv", |b| {
        b.iter(|| run_block(async {
            for _ in 0..N {
                std::hint::black_box(tcp_client.call(PingRequest::Ping).await.unwrap());
            }
        }));
    });

    group.bench_function("vsock_rkyv", |b| {
        b.iter(|| run_block(async {
            for _ in 0..N {
                std::hint::black_box(vsock_client.call(PingRequest::Ping).await.unwrap());
            }
        }));
    });

    // group.bench_function("shm_bitcode", |b| {
    //     b.iter(|| run_block(async {
    //         for _ in 0..N {
    //             std::hint::black_box(shm_client.call(PingRequest::Ping).await.unwrap());
    //         }
    //     }));
    // });

    group.finish();
}

// ─── регистрация ──────────────────────────────────────────────────────────────

criterion_group! {
    name   = rpc_throughput_benches;
    config = Criterion::default();
    targets = bench_sequential_calls, bench_concurrent_calls, bench_bulk_comparison
}
criterion_main!(rpc_throughput_benches);