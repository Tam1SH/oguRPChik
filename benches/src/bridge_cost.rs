
mod common;

use compio::BufResult;
use compio::io::compat::AsyncStream;
use compio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use criterion::{Criterion, criterion_group, criterion_main};
use futures_util::{AsyncReadExt as _, AsyncWriteExt as _};

const PAYLOAD: usize = 1024;

async fn raw_roundtrip(client: &mut ogurpchik::net::Conn) {
    let BufResult(res, _) = client.write_all(vec![0xAB; PAYLOAD]).await;
    res.expect("write failed");
    let BufResult(res, _) = client.read_exact(vec![0u8; PAYLOAD]).await;
    res.expect("read failed");
}

async fn bridged_roundtrip(client: &mut AsyncStream<ogurpchik::net::Conn>) {
    client.write_all(&[0xAB; PAYLOAD]).await.expect("write failed");
    client
        .read_exact(&mut [0u8; PAYLOAD])
        .await
        .expect("read failed");
}

fn bench_bridge_cost(c: &mut Criterion) {
    let runtime = compio::runtime::Runtime::new().expect("runtime");
    let mut group = c.benchmark_group("bridge_cost_1kib_roundtrip");

    for &kind in common::TRANSPORTS {
        let (listener, connect) = runtime.block_on(common::bind(kind));

        let (echo_listener, echo_connect) = (listener, connect);
        compio::runtime::spawn(async move {
            let mut conn = echo_listener.accept().await.expect("accept failed");
            loop {
                let BufResult(res, buf) = conn.read_exact(vec![0u8; PAYLOAD]).await;
                match res {
                    Ok(()) => {
                        let BufResult(res, _) = conn.write_all(buf).await;
                        if res.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .detach();
        let mut raw_client = runtime.block_on(echo_connect().connect());

        group.bench_function(format!("{kind}/raw_conn"), |b| {
            b.iter(|| runtime.block_on(raw_roundtrip(&mut raw_client)));
        });

        let (listener2, connect2) = runtime.block_on(common::bind(kind));
        compio::runtime::spawn(async move {
            let conn = listener2.accept().await.expect("accept failed");
            let mut conn = AsyncStream::new(conn);
            let mut buf = [0u8; PAYLOAD];
            loop {
                if conn.read_exact(&mut buf).await.is_err() {
                    break;
                }
                if conn.write_all(&buf).await.is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut bridged_client = AsyncStream::new(runtime.block_on(connect2().connect()));

        group.bench_function(format!("{kind}/async_stream"), |b| {
            b.iter(|| runtime.block_on(bridged_roundtrip(&mut bridged_client)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_bridge_cost);
criterion_main!(benches);
