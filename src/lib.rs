extern crate core;

pub mod server;
pub mod client;
pub mod client_per_core;
pub mod main_loop;
pub mod message_codec;
pub mod rkyv_protocol;
pub mod runtime;
pub mod utils;
pub mod worker;
pub mod tpc_pool;
pub mod transport;

mod align_buffer;

use crate::message_codec::MessageCodec;

pub trait ServiceHandler<C: MessageCodec>: Clone + Send + Sync + 'static {
    async fn on_request(&self, req: &C::RequestView) -> anyhow::Result<C::Response>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::setup;
    use crate::client::{Client, Priority};
    use crate::rkyv_protocol::RkyvProtocol;
    use crate::transport::stream::adapters::tcp::TcpTransport;
    use std::ops::Deref;
    use std::time::Duration;
    use rkyv::{Archive, Deserialize, Serialize};
    use crate::align_buffer::AlignedBuffer;
    use crate::transport::base::TransportBuilder;
    use crate::transport::impls::peer::PeerConfig;
    use crate::transport::stream::adapters::shm::ShmTransport;
    use crate::transport::stream::adapters::vsock::VsockTransport;
    use crate::transport::topology::RpcTopologyRegistry;

    #[derive(Archive, Deserialize, Serialize, Debug)]
    #[rkyv(derive(Debug, PartialEq, Eq))]
    pub enum Request {
        Ping,
    }

    #[derive(Archive, Deserialize, Serialize, Debug, PartialEq)]
    #[rkyv(derive(Debug, PartialEq, Eq))]
    pub enum Response {
        Pong,
    }
    
    #[derive(Clone)]
    struct EchoHandler;
    impl ServiceHandler<RkyvProtocol<Request, Response>> for EchoHandler {
        async fn on_request(&self, req: &ArchivedRequest) -> anyhow::Result<Response> {
            match req {
                ArchivedRequest::Ping => Ok(Response::Pong),

                _ => Err(anyhow::anyhow!("not a ping")),
            }
        }
    }

    async fn run_rpc_test<T>(transport: T) -> anyhow::Result<()>
    where
        T: TransportBuilder<AlignedBuffer> + Send + Sync + Clone + 'static
    {
        let num_cores = 2;

        runtime::init(num_cores);

        let codec_name = RkyvProtocol::<Request, Response>::kind();
        let registry = RpcTopologyRegistry::new(num_cores, transport.kind(), codec_name.to_string());

        let srv_reg = registry.clone();
        let srv_trans = transport.clone();

        compio::runtime::spawn(async move {
            setup()
                .with_transport(srv_trans)
                .with_registry(srv_reg)
                .cores(num_cores)
                .service(EchoHandler)
                .run()
                .await
                .expect("Failed to start server");
        }).detach();


        let topology = registry.ready().await;

        let client = Client::<RkyvProtocol<Request, Response>, _>::connect(transport, topology)
            .await
            .expect("Failed to connect client");

        let res = client
            .call(Request::Ping, Priority::Normal)
            .await
            .expect("Call failed");

        assert_eq!(*res.deref(), ArchivedResponse::Pong);
        Ok(())
    }

    #[compio::test]
    async fn test_rpc_tcp() -> anyhow::Result<()> {
        use crate::transport::stream::adapters::tcp::TcpTransport;

        let transport = TcpTransport::new("127.0.0.1".to_string(), 0, PeerConfig::default());
        run_rpc_test(transport).await
    }

    #[compio::test]
    async fn test_rpc_vsock() -> anyhow::Result<()> {

        let transport = VsockTransport::new(0, 5000, PeerConfig::default());
        run_rpc_test(transport).await
    }

    #[compio::test]
    async fn test_rpc_shm() -> anyhow::Result<()> {

        let service_base_name = format!("test_shm_{}", std::process::id());
        let transport = ShmTransport::new(&service_base_name);

        run_rpc_test(transport).await
    }

}

#[cfg(test)]
mod bench_tests {
    use super::*;
    use crate::server::setup;
    use crate::client::{Client, Priority};
    use crate::rkyv_protocol::RkyvProtocol;
    use crate::transport::stream::adapters::tcp::TcpTransport;
    use futures::stream::FuturesUnordered;
    use futures::StreamExt;
    use hdrhistogram::Histogram;
    use rkyv::{Archive, Deserialize, Serialize};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use crate::transport::base::TransportBuilder;
    use crate::transport::impls::peer::PeerConfig;
    use crate::transport::topology::RpcTopologyRegistry;

    #[derive(Archive, Deserialize, Serialize, Debug)]
    #[rkyv(derive(Debug, PartialEq, Eq))]
    pub enum Request {
        Ping,
        SmallTask(u64),
        BigData(Vec<u8>),
    }

    #[derive(Archive, Deserialize, Serialize, Debug, PartialEq)]
    #[rkyv(derive(Debug, PartialEq, Eq))]
    pub enum Response {
        Pong,
        Processed(u64),
        DataReceived(usize),
    }

    #[derive(Clone)]
    struct EchoHandler;
    impl ServiceHandler<RkyvProtocol<Request, Response>> for EchoHandler {
        async fn on_request(&self, req: &ArchivedRequest) -> anyhow::Result<Response> {
            match req {
                ArchivedRequest::Ping => Ok(Response::Pong),
                ArchivedRequest::SmallTask(val) => Ok(Response::Processed(u64::from(*val))),
                ArchivedRequest::BigData(data) => Ok(Response::DataReceived(data.len())),
            }
        }
    }

    #[compio::test]
    async fn bench_rpc_comprehensive() {
        runtime::init(num_cpus::get());

        let test_duration = Duration::from_secs(10);

        let transport = TcpTransport::new("127.0.0.1".to_string(), 0, PeerConfig::default());

        let codec_name = RkyvProtocol::<Request, Response>::kind();

        let registry = RpcTopologyRegistry::new(num_cpus::get(), transport.kind(), codec_name.to_string());

        let srv_reg = registry.clone();

        let srv_trans = transport.clone();

        compio::runtime::spawn(async move {
            let Err(e) = setup()
                .cores(num_cpus::get())
                .service(EchoHandler)
                .with_transport(srv_trans)
                .with_registry(srv_reg)
                .run()
                .await;

            eprintln!("Server error: {}", e);
        })
            .detach();

        compio::time::sleep(Duration::from_millis(500)).await;

        let topology = registry.ready().await;

        let client =
            Client::<RkyvProtocol<Request, Response>, _>::connect(transport, topology)
                .await
                .expect("Failed to connect FatClient");

        let critical_counter = Arc::new(AtomicU64::new(0));
        let normal_counter = Arc::new(AtomicU64::new(0));
        let bulk_counter = Arc::new(AtomicU64::new(0));
        let bulk_bytes = Arc::new(AtomicU64::new(0));

        let mut critical_hist = Histogram::<u64>::new_with_bounds(100, 1_000_000_000, 3).unwrap();

        let normal_concurrency = 12;
        for _ in 0..normal_concurrency {
            let client_clone = client.clone();
            let counter = normal_counter.clone();
            compio::runtime::spawn(async move {
                let mut futures = FuturesUnordered::new();
                for _ in 0..12 {
                    futures.push(client_clone.call(Request::SmallTask(42), Priority::Normal));
                }
                while let Some(res) = futures.next().await {
                    if res.is_ok() {
                        counter.fetch_add(1, Ordering::Relaxed);
                        futures.push(client_clone.call(Request::SmallTask(42), Priority::Normal));
                    }
                }
            })
                .detach();
        }

        let bulk_concurrency = 32;
        let large_payload = vec![0xAAu8; 64 * 1024];
        for _ in 0..bulk_concurrency {
            let client_clone = client.clone();
            let counter = bulk_counter.clone();
            let bytes = bulk_bytes.clone();
            let payload = large_payload.clone();
            compio::runtime::spawn(async move {
                let mut futures = FuturesUnordered::new();
                for _ in 0..5 {
                    futures
                        .push(client_clone.call(Request::BigData(payload.clone()), Priority::Bulk));
                }
                while let Some(res) = futures.next().await {
                    if res.is_ok() {
                        counter.fetch_add(1, Ordering::Relaxed);
                        bytes.fetch_add(payload.len() as u64, Ordering::Relaxed);
                        futures.push(
                            client_clone.call(Request::BigData(payload.clone()), Priority::Bulk),
                        );
                    }
                }
            })
                .detach();
        }

        println!("🔥 Warm-up (2 sec)...");
        compio::time::sleep(Duration::from_secs(2)).await;

        println!(
            "🚀 Benchmarking (Duration: {:?}, Cores: {})...",
            test_duration,
            runtime::core_count()
        );

        let start_test = Instant::now();

        while start_test.elapsed() < test_duration {
            let now = Instant::now();
            let res = client.call(Request::Ping, Priority::Critical).await;

            if let Ok(_) = res {
                let rtt = now.elapsed();
                critical_hist.record(rtt.as_nanos() as u64).unwrap();
                critical_counter.fetch_add(1, Ordering::Relaxed);
            }
        }

        let total_duration = start_test.elapsed().as_secs_f64();

        let crit_total = critical_counter.load(Ordering::Relaxed);
        let norm_total = normal_counter.load(Ordering::Relaxed);
        let bulk_total = bulk_counter.load(Ordering::Relaxed);
        let bulk_mb = bulk_bytes.load(Ordering::Relaxed) as f64 / 1024.0 / 1024.0;

        println!("\n{}", "=".repeat(60));
        println!("📊 FINAL RPC PERFORMANCE REPORT");
        println!("{}", "=".repeat(60));

        println!("CRITICAL (Latency Oriented):");
        println!("  Total:      {} req", crit_total);
        println!(
            "  RPS:        {:.2} req/sec",
            crit_total as f64 / total_duration
        );
        println!(
            "  P50 Latency: {:>10?}",
            Duration::from_nanos(critical_hist.value_at_quantile(0.5))
        );
        println!(
            "  P99 Latency: {:>10?}",
            Duration::from_nanos(critical_hist.value_at_quantile(0.99))
        );
        println!(
            "  Max Latency: {:>10?}",
            Duration::from_nanos(critical_hist.max())
        );

        println!("\nNORMAL (Balanced):");
        println!("  Total:      {} req", norm_total);
        println!(
            "  RPS:        {:.2} req/sec",
            norm_total as f64 / total_duration
        );

        println!("\nBULK (Throughput Oriented):");
        println!("  Total:      {} req", bulk_total);
        println!(
            "  RPS:        {:.2} req/sec",
            bulk_total as f64 / total_duration
        );
        println!("  Bandwidth:  {:.2} MB/sec", bulk_mb / total_duration);

        println!("{}", "=".repeat(60));
        println!(
            "Total Aggregated RPS: {:.2} req/sec",
            (crit_total + norm_total + bulk_total) as f64 / total_duration
        );
        println!("{}", "=".repeat(60));
    }

    #[compio::test]
    async fn bench_rpc_normal_stress() {
        runtime::init(num_cpus::get());

        let test_duration = Duration::from_secs(5);
        let num_cores = num_cpus::get();

        let transport = TcpTransport::new("127.0.0.1".to_string(), 0, PeerConfig::default());

        let codec_name = RkyvProtocol::<Request, Response>::kind();

        let registry = RpcTopologyRegistry::new(num_cpus::get(), transport.kind(), codec_name.to_string());

        let srv_reg = registry.clone();

        let srv_trans = transport.clone();

        compio::runtime::spawn(async move {
            let _ = setup()
                .cores(num_cores)
                .service(EchoHandler)
                .with_transport(srv_trans)
                .with_registry(srv_reg)
                .run()
                .await;
        })
            .detach();

        compio::time::sleep(Duration::from_millis(500)).await;

        let topology = registry.ready().await;

        let client =
            Client::<RkyvProtocol<Request, Response>, _>::connect(transport, topology)
                .await
                .expect("Failed to connect FatClient");

        let normal_counter = Arc::new(AtomicU64::new(0));
        let error_counter = Arc::new(AtomicU64::new(0));

        let hist = Arc::new(std::sync::Mutex::new(
            Histogram::<u64>::new_with_bounds(100, 1_000_000_000, 3).unwrap(),
        ));

        let concurrency_per_lane = 4;
        let num_normal_lanes = (num_cores - 1 + 1) / 2;
        let total_workers = num_normal_lanes * concurrency_per_lane;

        println!(
            "🔥 Initializing stress test with {} workers for NORMAL priority...",
            total_workers
        );

        for _ in 0..total_workers {
            let client_clone = client.clone();
            let counter = normal_counter.clone();
            let err_counter = error_counter.clone();
            let hist_clone = hist.clone();

            compio::runtime::spawn(async move {
                loop {
                    let start = Instant::now();
                    let res = client_clone
                        .call(Request::SmallTask(0), Priority::Normal)
                        .await;

                    let elapsed = start.elapsed().as_nanos() as u64;

                    match res {
                        Ok(_) => {
                            counter.fetch_add(1, Ordering::Relaxed);
                            let mut h = hist_clone.lock().unwrap();
                            let _ = h.record(elapsed);
                        }
                        Err(_) => {
                            err_counter.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            })
                .detach();
        }

        println!("🚀 Warming up (3 sec)...");
        compio::time::sleep(Duration::from_secs(3)).await;

        normal_counter.store(0, Ordering::SeqCst);
        {
            let mut h = hist.lock().unwrap();
            h.reset();
        }

        println!("⚡ Starting measurements for {:?}...", test_duration);
        let start_time = Instant::now();
        compio::time::sleep(test_duration).await;
        let actual_duration = start_time.elapsed();

        let total_requests = normal_counter.load(Ordering::SeqCst);
        let total_errors = error_counter.load(Ordering::SeqCst);
        let rps = total_requests as f64 / actual_duration.as_secs_f64();

        let h = hist.lock().unwrap();

        println!("\n{}", "=".repeat(60));
        println!("📊 NORMAL PRIORITY STRESS REPORT");
        println!("{}", "=".repeat(60));
        println!("Cores used:          {}", num_cores);
        println!("Normal Lanes:        {}", num_normal_lanes);
        println!("Workers:             {}", total_workers);
        println!("Duration:            {:.2?}", actual_duration);
        println!("{}", "-".repeat(60));
        println!("Throughput:");
        println!("  Total Requests:    {}", total_requests);
        println!("  RPS:               {:.0} req/sec", rps);
        println!("  Errors:            {}", total_errors);
        println!("{}", "-".repeat(60));
        println!("Latency:");
        println!(
            "  P50:               {:?}",
            Duration::from_nanos(h.value_at_quantile(0.5))
        );
        println!(
            "  P90:               {:?}",
            Duration::from_nanos(h.value_at_quantile(0.9))
        );
        println!(
            "  P99:               {:?}",
            Duration::from_nanos(h.value_at_quantile(0.99))
        );
        println!(
            "  P99.9:             {:?}",
            Duration::from_nanos(h.value_at_quantile(0.999))
        );
        println!("  Max:               {:?}", Duration::from_nanos(h.max()));
        println!("{}", "=".repeat(60));
    }
}

