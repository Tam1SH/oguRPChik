use criterion::{BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use std::path::PathBuf;
use std::time::Duration;
use ogurpchik::transport::base::{MessageSink, MessageSource};

#[path = "common.rs"]
mod common;
use common::{make_vsock_peer_pair, make_tcp_peer_pair, make_uds_peer_pair};

const SMALL_SIZES: &[usize] = &[16, 64, 256, 512, 1_024];

fn uds_path() -> PathBuf {
    std::env::temp_dir().join(format!("ogurpchik_bench_{}.sock", std::process::id()))
}


fn bench_rtt_vsock(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtt_vsock");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(200);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(2));

    for &size in SMALL_SIZES {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &sz| {
            let runtime = compio::runtime::Runtime::new().unwrap();
            b.iter_custom(|iters| {
                runtime.block_on(async {
                    let (tx, mut rx) = make_vsock_peer_pair(9400 + sz as u32 % 1000).await;
                    let start = std::time::Instant::now();
                    for _ in 0..iters {
                        tx.send(vec![0u8; sz].into()).await.unwrap();
                        std::hint::black_box(rx.recv().await.unwrap());
                    }
                    start.elapsed()
                })
            });
        });
    }

    group.finish();
}

fn bench_rtt_tcp(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtt_tcp");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(200);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(2));

    for &size in SMALL_SIZES {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &sz| {
            let runtime = compio::runtime::Runtime::new().unwrap();
            b.iter_custom(|iters| {
                runtime.block_on(async {
                    let (tx, mut rx) = make_tcp_peer_pair().await;
                    let start = std::time::Instant::now();
                    for _ in 0..iters {
                        tx.send(vec![0u8; sz].into()).await.unwrap();
                        std::hint::black_box(rx.recv().await.unwrap());
                    }
                    start.elapsed()
                })
            });
        });
    }

    group.finish();
}

fn bench_rtt_uds(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtt_uds");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(200);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(2));

    for &size in SMALL_SIZES {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &sz| {
            let runtime = compio::runtime::Runtime::new().unwrap();
            let path = uds_path();
            b.iter_custom(|iters| {
                runtime.block_on(async {
                    let (tx, mut rx) = make_uds_peer_pair(&path).await;
                    let start = std::time::Instant::now();
                    for _ in 0..iters {
                        tx.send(vec![0u8; sz].into()).await.unwrap();
                        std::hint::black_box(rx.recv().await.unwrap());
                    }
                    start.elapsed()
                })
            });
        });
    }

    group.finish();
}


fn bench_first_byte(c: &mut Criterion) {
    let mut group = c.benchmark_group("first_byte");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(100);

    group.bench_function("tcp", |b| {
        let runtime = compio::runtime::Runtime::new().unwrap();
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let (tx, mut rx) = make_tcp_peer_pair().await;
                    let start = std::time::Instant::now();
                    tx.send(vec![1u8; 64].into()).await.unwrap();
                    std::hint::black_box(rx.recv().await.unwrap());
                    total += start.elapsed();
                }
                total
            })
        });
    });

    group.bench_function("uds", |b| {
        let runtime = compio::runtime::Runtime::new().unwrap();
        let path = uds_path();
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let (tx, mut rx) = make_uds_peer_pair(&path).await;
                    let start = std::time::Instant::now();
                    tx.send(vec![1u8; 64].into()).await.unwrap();
                    std::hint::black_box(rx.recv().await.unwrap());
                    total += start.elapsed();
                }
                total
            })
        });
    });

    group.finish();
}

fn bench_backpressure(c: &mut Criterion) {
    let mut group = c.benchmark_group("backpressure");
    group.sampling_mode(SamplingMode::Linear);
    group.sample_size(50);

    const FLOOD_COUNT: usize = 128;
    group.throughput(Throughput::Elements(FLOOD_COUNT as u64));

    group.bench_function("tcp", |b| {
        let runtime = compio::runtime::Runtime::new().unwrap();
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let (tx, mut rx) = make_tcp_peer_pair().await;
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    let sender = compio::runtime::spawn({
                        let tx = tx.clone();
                        async move {
                            for i in 0..FLOOD_COUNT {
                                tx.send(vec![i as u8; 256].into()).await.unwrap();
                            }
                        }
                    });
                    for _ in 0..FLOOD_COUNT {
                        std::hint::black_box(rx.recv().await.unwrap());
                    }
                    sender.await.unwrap();
                }
                start.elapsed()
            })
        });
    });

    group.bench_function("uds", |b| {
        let runtime = compio::runtime::Runtime::new().unwrap();
        let path = uds_path();
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let (tx, mut rx) = make_uds_peer_pair(&path).await;
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    let sender = compio::runtime::spawn({
                        let tx = tx.clone();
                        async move {
                            for i in 0..FLOOD_COUNT {
                                tx.send(vec![i as u8; 256].into()).await.unwrap();
                            }
                        }
                    });
                    for _ in 0..FLOOD_COUNT {
                        std::hint::black_box(rx.recv().await.unwrap());
                    }
                    sender.await.unwrap();
                }
                start.elapsed()
            })
        });
    });

    group.finish();
}

criterion_group! {
    name   = latency_benches;
    config = Criterion::default();
    targets = bench_rtt_vsock, bench_rtt_tcp, bench_rtt_uds,
              bench_first_byte, bench_backpressure
}
criterion_main!(latency_benches);