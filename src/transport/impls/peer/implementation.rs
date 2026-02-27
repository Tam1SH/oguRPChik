use crate::align_buffer::AlignedBuffer;
use crate::client::Priority;
use crate::tpc_pool::{InnerPool, Mixed, TpcPool};
use crate::transport::stream::AsyncStream;
use bytes::BytesMut;
use compio::buf::{IntoInner, IoBuf, IoBufMut, IoVectoredBufMut, SetLen, Slice};
use compio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use compio::BufResult;
use futures::{FutureExt, SinkExt, StreamExt};
use std::io;
use tracing::{debug, error, info, instrument, trace};
use crate::transport::impls::peer::handle::{OutgoingMsg, PeerSink, PeerSource};
use local_sync::mpsc;
use crate::transport::impls::peer::config::PeerConfig;

pub struct Peer;

impl Peer {
    #[instrument(skip_all)]
    pub fn new<S: AsyncStream + Clone + 'static>(
        stream: S,
        config: PeerConfig,
    ) -> io::Result<(PeerSink, PeerSource)> {
        let (outgoing_tx, outgoing_rx) = mpsc::bounded::channel(config.channel_size);
        let (incoming_tx, incoming_rx) = mpsc::bounded::channel(config.channel_size);

        let (writer, reader) = stream.split();

        compio::runtime::spawn(Self::run_writer(writer, outgoing_rx, config.clone())).detach();

        compio::runtime::spawn(Self::run_reader(reader, incoming_tx, config)).detach();

        Ok((PeerSink { outgoing_tx }, PeerSource { incoming_rx }))
    }

    #[instrument(skip_all, name = "peer_writer")]
    async fn run_writer<W: AsyncWrite>(
        mut writer: W,
        mut outgoing_rx: mpsc::bounded::Rx<OutgoingMsg>,
        config: PeerConfig,
    ) -> anyhow::Result<()> {
        debug!("Writer worker started");

        let mut batch = Vec::with_capacity(config.batch_limit * 2);

        while let Some(msg_enum) = outgoing_rx.recv().await {
            TpcPool::with(|pool| {
                let add_to_batch = |p: &mut InnerPool, b: &mut Vec<Mixed>, msg: AlignedBuffer| {
                    let header = p.acquire_header(msg.0.len());
                    b.push(Mixed::Bytes(header));
                    b.push(Mixed::AlignedBuffer(msg));
                };

                match msg_enum {
                    OutgoingMsg::Single(msg) => add_to_batch(pool, &mut batch, msg),
                    OutgoingMsg::Batch(msgs) => {
                        for msg in msgs {
                            add_to_batch(pool, &mut batch, msg);
                        }
                    }
                }

                while batch.len() < config.batch_limit * 2 {
                    match outgoing_rx.try_recv() {
                        Ok(OutgoingMsg::Single(msg)) => add_to_batch(pool, &mut batch, msg),
                        Ok(OutgoingMsg::Batch(msgs)) => {
                            for msg in msgs {
                                add_to_batch(pool, &mut batch, msg);
                            }
                        }
                        Err(_) => break,
                    }
                }
            });

            trace!(
                msgs_count = batch.len() / 2,
                total_iovs = batch.len(),
                "Sending vectored batch"
            );

            let BufResult(res, returned_batch) = writer.write_vectored_all(batch).await;

            if let Err(e) = res {
                error!(error = ?e, "Failed to write batch to stream");
                return Err(e.into());
            }

            batch = returned_batch;

            TpcPool::with(|pool| {
                for buf in batch.drain(..) {
                    pool.release_mixed(buf);
                }
            });
        }

        info!("Writer worker exiting");
        Ok(())
    }

    #[instrument(skip_all, name = "peer_reader")]
    pub async fn run_reader<R: AsyncRead>(
        mut reader: R,
        mut incoming_tx: mpsc::bounded::Tx<AlignedBuffer>,
        config: PeerConfig,
    ) -> anyhow::Result<()> {
        debug!("Reader worker started");

        let mut buffer = TpcPool::acquire_body(config.read_buffer_capacity);

        loop {
            if buffer.len() == buffer.0.capacity() {
                let current_cap = buffer.0.capacity();
                let new_cap = if current_cap == 0 {
                    4096
                } else {
                    current_cap * 2
                };

                if new_cap > 100 * 1024 * 1024 {
                    return Err(anyhow::anyhow!("Buffer limit exceeded"));
                }

                let mut new_buf = TpcPool::acquire_body(new_cap);

                unsafe {
                    let len = buffer.len();
                    new_buf.set_len(len);
                    if len > 0 {
                        std::ptr::copy_nonoverlapping(buffer.as_ptr(), new_buf.as_mut_ptr(), len);
                    }
                }

                TpcPool::release_body(buffer);
                buffer = new_buf;
            }

            let prev_len = buffer.len();

            let BufResult(res, returned_buf) = reader.read(buffer).await;
            buffer = returned_buf;

            let n = match res {
                Ok(0) => {
                    info!("Reader reached EOF");
                    break;
                }
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };

            unsafe { buffer.set_len(prev_len + n) };

            let mut offset = 0;
            loop {
                let available = buffer.len() - offset;

                if available < 4 {
                    break;
                }

                let len_slice = &buffer.0[offset..offset + 4];
                let msg_len = u32::from_le_bytes(len_slice.try_into().unwrap()) as usize;

                let total_frame_len = 4 + msg_len;

                if available < total_frame_len {
                    break;
                }

                let mut payload = TpcPool::acquire_body(msg_len);
                unsafe {
                    payload.set_len(msg_len);

                    std::ptr::copy_nonoverlapping(
                        buffer.as_ptr().add(offset + 4),
                        payload.as_mut_ptr(),
                        msg_len,
                    );
                }

                if incoming_tx.send(payload).await.is_err() {
                    return Err(anyhow::anyhow!("Channel closed"));
                }

                offset += total_frame_len;
            }

            if offset > 0 {
                let remaining = buffer.len() - offset;
                if remaining > 0 {
                    unsafe {
                        let ptr = buffer.as_mut_ptr();

                        std::ptr::copy(ptr.add(offset), ptr, remaining);
                    }
                }
                unsafe { buffer.set_len(remaining) };
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::stream::vsock::general::{VListener, VStream};
    use rkyv::util::AlignedVec;
    use tracing::Level;
    use crate::transport::stream::vsock::VsockTarget;

    async fn setup_vsock_pair(port: u32) -> (VStream, VStream) {
        let listener = VListener::bind_loopback(port).expect("Vsock bind failed");

        let server_handle =
            compio::runtime::spawn(
                async move { listener.accept().await.expect("Vsock accept failed") },
            );

        let client_stream = VStream::connect(VsockTarget::Cid(0), port)
            .await
            .expect("Vsock connect failed");

        let (server_stream, _) = server_handle.await.unwrap();

        (server_stream, client_stream)
    }

    #[compio::test]
    async fn test_peer_simple_delivery() {
        let (server_stream, client_stream) = setup_vsock_pair(8000).await;

        let (mut s_handle, _) = Peer::new(server_stream, PeerConfig::default()).unwrap();
        let (_, PeerSource { incoming_rx: mut c_rx }) = Peer::new(client_stream, PeerConfig::default()).unwrap();

        let msg = vec![1u8, 3u8, 3u8, 7u8];
        let mut a = AlignedVec::with_capacity(4);
        a.extend_from_slice(&msg[..]);

        s_handle.send(AlignedBuffer(a.clone())).await.unwrap();

        let received = c_rx.recv().await.expect("No message received");
        assert_eq!(received.0.as_slice(), a.as_slice());
    }

    #[compio::test]
    async fn test_peer_large_packet() {
        let (server_stream, client_stream) = setup_vsock_pair(8001).await;

        let (s_handle, _s_rx) = Peer::new(server_stream, PeerConfig::default()).unwrap();
        let (_c_handle, PeerSource { incoming_rx: mut c_rx }) = Peer::new(client_stream, PeerConfig::default()).unwrap();

        let large_data = vec![0xAAu8; 1024 * 1024];
        let mut a = AlignedVec::with_capacity(1024 * 1024);
        a.extend_from_slice(&large_data[..]);

        s_handle.send(AlignedBuffer(a.clone())).await.unwrap();

        let received = c_rx.recv().await.expect("No message received");
        assert_eq!(received.0.len(), large_data.len());
        assert_eq!(received.0.as_slice(), a.as_slice());
    }

    #[compio::test]
    async fn test_peer_full_duplex_bidirectional() {
        // tracing_subscriber::fmt()
        //     .with_max_level(Level::TRACE)
        //     .init();

        let (server_stream, client_stream) = setup_vsock_pair(8002).await;

        let (mut s_tx, PeerSource { incoming_rx: mut s_rx }) = Peer::new(server_stream, PeerConfig::default()).unwrap();
        let (mut c_tx, PeerSource { incoming_rx: mut c_rx }) = Peer::new(client_stream, PeerConfig::default()).unwrap();

        let mut s_data = vec![0x11u8; 64];
        let mut c_data = vec![0x22u8; 64];

        s_data[0..8].copy_from_slice(&0xDEADBEEF_00000001u64.to_le_bytes());
        s_data[56..64].copy_from_slice(&0xDEADBEEF_00000002u64.to_le_bytes());

        c_data[0..8].copy_from_slice(&0xDEADBEEF_00000001u64.to_le_bytes());
        c_data[56..64].copy_from_slice(&0xDEADBEEF_00000002u64.to_le_bytes());

        let (res_c, res_s) = futures::join!(
            async {
                let mut a = AlignedVec::with_capacity(c_data.len());
                a.extend_from_slice(&c_data);
                c_tx.send(AlignedBuffer(a)).await.unwrap();
                c_rx.recv().await.unwrap()
            },
            async {
                let mut a = AlignedVec::with_capacity(s_data.len());
                a.extend_from_slice(&s_data);
                s_tx.send(AlignedBuffer(a)).await.unwrap();
                s_rx.recv().await.unwrap()
            }
        );

        assert_eq!(res_c.0.as_slice(), s_data.as_slice());
        assert_eq!(res_s.0.as_slice(), c_data.as_slice());
    }

    #[compio::test]
    async fn test_peer_multiple_messages_order() {
        let (server_stream, client_stream) = setup_vsock_pair(8003).await;
        let (mut s_tx, _) = Peer::new(server_stream, PeerConfig::default()).unwrap();
        let (_, PeerSource { incoming_rx: mut c_rx }) = Peer::new(client_stream, PeerConfig::default()).unwrap();

        let counts = [10, 20, 30];

        for &len in &counts {
            let mut a = AlignedVec::with_capacity(len);
            a.extend_from_slice(&vec![len as u8; len]);
            s_tx.send(AlignedBuffer(a)).await.unwrap();
        }

        for &len in &counts {
            let received = c_rx.recv().await.expect("Failed to receive");
            assert_eq!(received.0.len(), len);
            assert!(received.0.iter().all(|&b| b == len as u8));
        }
    }

    #[compio::test]
    async fn test_peer_zero_length_packet() {
        let (server_stream, client_stream) = setup_vsock_pair(8004).await;
        let (mut s_tx, _) = Peer::new(server_stream, PeerConfig::default()).unwrap();
        let (_, PeerSource { incoming_rx: mut c_rx }) = Peer::new(client_stream, PeerConfig::default()).unwrap();

        s_tx.send(AlignedBuffer(AlignedVec::new())).await.unwrap();

        let mut a = AlignedVec::with_capacity(3);
        a.extend_from_slice(&[1, 2, 3]);
        s_tx.send(AlignedBuffer(a)).await.unwrap();

        let received = c_rx.recv().await.unwrap();

        if received.0.is_empty() {
            let second = c_rx.recv().await.unwrap();
            assert_eq!(second.0.as_slice(), &[1, 2, 3]);
        } else {
            assert_eq!(received.0.as_slice(), &[1, 2, 3]);
        }
    }

    #[compio::test]
    async fn test_peer_varying_sizes_exceeding_buffer() {
        let (server_stream, client_stream) = setup_vsock_pair(8005).await;

        let mut config = PeerConfig::default();
        config.read_buffer_capacity = 4096;

        let (s_handle, _) = Peer::new(server_stream, PeerConfig::default()).unwrap();
        let (_, PeerSource { incoming_rx: mut c_rx }) = Peer::new(client_stream, config).unwrap();

        let sizes = vec![100, 4000, 5000, 64 * 1024, 1024 * 1024];

        for &size in &sizes {
            let mut data = AlignedVec::with_capacity(size);

            let content: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            data.extend_from_slice(&content);

            debug!("Sending message of size {}", size);
            s_handle.send(AlignedBuffer(data)).await.unwrap();
        }

        for &size in &sizes {
            let received = c_rx.recv().await.expect("Failed to receive message");

            debug!("Received message of len {}", received.0.len());

            assert_eq!(
                received.0.len(),
                size,
                "Message size mismatch. Expected {}, got {}",
                size,
                received.0.len()
            );

            let expected: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            assert_eq!(
                received.0.as_slice(),
                expected.as_slice(),
                "Content mismatch for size {}",
                size
            );
        }
    }
}

// #[cfg(test)]
// mod bench_peer {
//     use super::*;
//     use compio::net::{TcpListener, TcpStream};
//     use std::sync::atomic::{AtomicU64, Ordering};
//     use std::sync::Arc;
//     use std::time::{Duration, Instant};
//     use crate::tpc_pool::PoolConfig;
// 
//     async fn create_peer_pair(
//         config: PeerConfig,
//     ) -> (
//         PeerSink,
//         mpsc::bounded::Rx<AlignedBuffer>,
//         PeerSink,
//         mpsc::bounded::Rx<AlignedBuffer>,
//     ) {
//         let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
//         let addr = listener.local_addr().unwrap();
// 
//         let conf = config.clone();
//         let server_task = compio::runtime::spawn(async move {
//             let (stream, _) = listener.accept().await.unwrap();
//             Peer::new(stream, conf).unwrap()
//         });
// 
//         let client_stream = TcpStream::connect(addr).await.unwrap();
//         let (c_handle, PeerSource { incoming_rx: mut c_rx }) = Peer::new(client_stream, config).unwrap();
//         let (s_handle, PeerSource { incoming_rx: mut s_rx }) = server_task.await.unwrap();
// 
//         (c_handle, c_rx, s_handle, s_rx)
//     }
// 
//     #[cfg(feature = "dhat-heap")]
//     #[global_allocator]
//     static ALLOC: dhat::Alloc = dhat::Alloc;
// 
//     #[compio::test]
//     async fn stress_test_peer_throughput() {
//         #[cfg(feature = "dhat-heap")]
//         let _profiler = dhat::Profiler::new_heap();
// 
//         TpcPool::init(PoolConfig::stress());
// 
//         let duration = Duration::from_secs(10);
// 
//         let msg_sizes = [64, 512, 4096, 16384, 65536];
// 
//         let config = PeerConfig {
//             priority: Priority::Normal,
//             channel_size: 1024,
//             batch_limit: 64,
//             read_buffer_capacity: 512 * 1024,
//         };
// 
//         let (c_handle, _c_rx, _s_handle, mut s_rx) = create_peer_pair(config).await;
// 
//         let total_msgs = Arc::new(AtomicU64::new(0));
//         let total_bytes = Arc::new(AtomicU64::new(0));
//         let total_latency_ns = Arc::new(AtomicU64::new(0));
//         let latency_samples = Arc::new(AtomicU64::new(0));
// 
//         let t_msgs = total_msgs.clone();
//         let t_bytes = total_bytes.clone();
//         let t_lat = total_latency_ns.clone();
//         let l_samples = latency_samples.clone();
// 
//         let start = Instant::now();
// 
//         compio::runtime::spawn(async move {
//             while let Some(msg) = s_rx.recv().await {
//                 t_msgs.fetch_add(1, Ordering::Relaxed);
//                 t_bytes.fetch_add(msg.0.len() as u64, Ordering::Relaxed);
// 
//                 if msg.0.len() >= 8 {
//                     let sent_at_nanos = u64::from_le_bytes(msg.0[..8].try_into().unwrap());
//                     if sent_at_nanos != 0 {
//                         let now_nanos = start.elapsed().as_nanos() as u64;
// 
//                         if now_nanos > sent_at_nanos {
//                             let latency = now_nanos - sent_at_nanos;
//                             t_lat.fetch_add(latency, Ordering::Relaxed);
//                             l_samples.fetch_add(1, Ordering::Relaxed);
//                         }
//                     }
//                 }
//                 TpcPool::release_body(msg);
//             }
//         })
//             .detach();
// 
// 
//         let start = Instant::now();
//         let num_producers = 1;
// 
//         for p_id in 0..num_producers {
//             let h = c_handle.clone();
//             let start_time = Instant::now();
// 
//             compio::runtime::spawn(async move {
//                 let mut iteration = 0u64;
//                 loop {
//                     iteration += 1;
//                     let msg_size = msg_sizes[(iteration as usize + p_id) % msg_sizes.len()];
// 
//                     let mut buf = TpcPool::acquire_body(msg_size);
//                     unsafe { buf.set_len(msg_size); }
// 
//                     if iteration % 100 == 0 {
//                         let ts = start_time.elapsed().as_nanos() as u64;
//                         buf.0[..8].copy_from_slice(&ts.to_le_bytes());
//                     } else {
//                         buf.0[..8].fill(0);
//                     }
// 
//                     if h.send(buf).await.is_err() {
//                         break;
//                     }
//                 }
//             })
//                 .detach();
//         }
// 
//         compio::time::sleep(duration).await;
// 
//         let total = total_msgs.load(Ordering::Acquire);
//         let bytes = total_bytes.load(Ordering::Acquire);
//         let elapsed = start.elapsed().as_secs_f64();
// 
//         println!("\n🚀 AGGRESSIVE PEER REPORT");
//         println!("RPS:          {:.2} req/sec", total as f64 / elapsed);
//         println!("Throughput:   {:.2} MB/sec", (bytes as f64 / 1024.0 / 1024.0) / elapsed);
//         println!("Total Msgs:   {}", total);
//         println!("Avg Msg Size: {} bytes", if total > 0 { bytes / total } else { 0 });
//         println!("---------------------------------");
//     }
// 
// }
