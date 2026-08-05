
mod common;

use criterion::{Criterion, criterion_group, criterion_main};

fn bench_rpc_latency(c: &mut Criterion) {
    let runtime = compio::runtime::Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("rpc_ping_latency");

    for &kind in common::TRANSPORTS {
        let (listener, connect) = runtime.block_on(common::bind(kind));
        common::spawn_server(listener);
        let session = runtime.block_on(common::client_session(connect()));

        group.bench_function(kind, |b| {
            b.iter(|| runtime.block_on(common::ping(&session)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_rpc_latency);
criterion_main!(benches);
