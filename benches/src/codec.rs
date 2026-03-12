use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

#[path = "common.rs"]
mod common;
use common::PingRequest;

use ogurpchik::codecs::base::{BufferAllocator, Envelope, MessageCodec};
use ogurpchik::codecs::rkyv_protocol::{AlignedBuffer, RkyvCodec};
use ogurpchik::pool::allocator::TpcAllocator;
use ogurpchik::transport::base::pool_config::PoolConfig;
use common::PongResponse;

type Codec = RkyvCodec<PingRequest, PongResponse>;

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec_encode");
    let alloc: TpcAllocator<AlignedBuffer> = BufferAllocator::get(&PoolConfig::default());

    group.bench_function("rkyv_request", |b| {
        let mut buf = alloc.allocate_hinted();
        b.iter(|| {
            Codec::encode(
                Envelope::Request { id: 1, payload: PingRequest::Ping },
                &mut buf,
            ).unwrap();
            std::hint::black_box(buf.len());
        });
    });

    group.bench_function("rkyv_response", |b| {
        let mut buf = alloc.allocate_hinted();
        b.iter(|| {
            Codec::encode(
                Envelope::Response { id: 1, payload: PongResponse::Pong },
                &mut buf,
            ).unwrap();
            std::hint::black_box(buf.len());
        });
    });

    group.finish();
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec_decode");
    let alloc: TpcAllocator<AlignedBuffer> = BufferAllocator::get(&PoolConfig::default());

    let req_bytes = {
        let mut buf = alloc.allocate_hinted();
        Codec::encode(Envelope::Request { id: 1, payload: PingRequest::Ping }, &mut buf).unwrap();
        buf.as_ref().to_vec()
    };
    let res_bytes = {
        let mut buf = alloc.allocate_hinted();
        Codec::encode(Envelope::Response { id: 1, payload: PongResponse::Pong }, &mut buf).unwrap();
        buf.as_ref().to_vec()
    };

    group.throughput(Throughput::Bytes(req_bytes.len() as u64));
    group.bench_function("rkyv_request", |b| {
        b.iter(|| std::hint::black_box(Codec::decode(&req_bytes).unwrap()));
    });

    group.throughput(Throughput::Bytes(res_bytes.len() as u64));
    group.bench_function("rkyv_response", |b| {
        b.iter(|| std::hint::black_box(Codec::decode(&res_bytes).unwrap()));
    });

    group.finish();
}

fn bench_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec_roundtrip");
    let alloc: TpcAllocator<AlignedBuffer> = BufferAllocator::get(&PoolConfig::default());

    group.bench_function("rkyv_req_res", |b| {
        let mut buf = alloc.allocate_hinted();
        b.iter(|| {
            Codec::encode(
                Envelope::Request { id: 1, payload: PingRequest::Ping },
                &mut buf,
            ).unwrap();
            let decoded = Codec::decode(buf.as_ref()).unwrap();
            std::hint::black_box(decoded);
        });
    });

    group.finish();
}

criterion_group!(codec_benches, bench_encode, bench_decode, bench_roundtrip);
criterion_main!(codec_benches);