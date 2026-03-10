use crate::transport::impls::peer::config::PeerConfig;
use crate::transport::impls::peer::handle::{OutgoingMsg, PeerSink, PeerSource};
use crate::transport::stream::{AsyncStream, StreamBuf};
use bytes::{BufMut, BytesMut};
use compio::BufResult;
use compio::buf::{IoBuf, IoBufMut, IoVectoredBufMut, SetLen, Slice};
use compio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use futures::{SinkExt, StreamExt};
use local_sync::mpsc;
use std::io;
use tracing::{debug, error, info, instrument, trace};
use crate::codecs::base::{BufferAllocator, OwnedBuf};
use crate::transport::impls::peer::frame::{Frame, FrameBatch};

pub struct Peer;


impl Peer {
    #[instrument(skip_all)]
    pub fn new<S, B, A>(
        stream: S,
        config: PeerConfig,
        strategy: A,
    ) -> io::Result<(PeerSink<B>, PeerSource<B>)>
    where
        S: AsyncStream + Clone + 'static,
        B: StreamBuf,
        A: BufferAllocator<Payload = B>,
    {
        let (outgoing_tx, outgoing_rx) = mpsc::bounded::channel(config.channel_size);
        let (incoming_tx, incoming_rx) = mpsc::bounded::channel(config.channel_size);
        let (writer, reader) = stream.split();

        compio::runtime::spawn(Self::run_writer::<_, B, A>(
            writer,
            outgoing_rx,
            config.clone(),
            strategy.clone(),
        ))
            .detach();

        compio::runtime::spawn(Self::run_reader::<_, B, A>(
            reader,
            incoming_tx,
            config,
            strategy,
        ))
            .detach();

        Ok((PeerSink { outgoing_tx }, PeerSource { incoming_rx }))
    }

    #[instrument(skip_all, name = "peer_writer")]
    async fn run_writer<W, B, A>(
        mut writer: W,
        mut outgoing_rx: mpsc::bounded::Rx<OutgoingMsg<B>>,
        config: PeerConfig,
        strategy: A,
    ) -> anyhow::Result<()>
    where
        W: AsyncWrite,
        B: StreamBuf,
        A: BufferAllocator<Payload = B>,
    {
        debug!("Writer worker started");

        let mut batch = FrameBatch::with_capacity(config.batch_limit);

        while let Some(msg_enum) = outgoing_rx.recv().await {
            let push = |b: &mut FrameBatch<B>, body: B| {
                let mut header = BytesMut::with_capacity(4);

                header.put_u32_le(body.len() as u32);
                b.push(Frame { header, body });
            };

            match msg_enum {
                OutgoingMsg::Single(msg) => push(&mut batch, msg),
                OutgoingMsg::Batch(msgs) => msgs.into_iter().for_each(|m| push(&mut batch, m)),
            }

            while batch.frame_count() < config.batch_limit {
                match outgoing_rx.try_recv() {
                    Ok(OutgoingMsg::Single(msg)) => push(&mut batch, msg),
                    Ok(OutgoingMsg::Batch(msgs)) => {
                        msgs.into_iter().for_each(|m| push(&mut batch, m))
                    }
                    Err(_) => break,
                }
            }

            trace!(frame_count = batch.frame_count(), "Sending vectored batch");

            let slots = batch.into_slots();
            let BufResult(res, returned_slots) = writer.write_vectored_all(slots).await;

            if let Err(e) = res {
                error!(error = ?e, "Failed to write batch");
                return Err(e.into());
            }

            batch = FrameBatch::from_slots(returned_slots);

            for frame in batch.drain_frames() {
                // header — 4 bytes, just drop it
                drop(frame.header);
                strategy.release(frame.body);
            }
        }

        info!("Writer worker exiting");
        Ok(())
    }

    #[instrument(skip_all, name = "peer_reader")]
    async fn run_reader<R, B, A>(
        mut reader: R,
        mut incoming_tx: mpsc::bounded::Tx<B>,
        config: PeerConfig,
        allocator: A,
    ) -> anyhow::Result<()>
    where
        R: AsyncRead,
        B: StreamBuf,
        A: BufferAllocator<Payload = B>,
    {
        debug!("Reader worker started");

        let mut buffer = allocator.allocate(config.read_buffer_capacity);

        loop {

            if buffer.len() == buffer.capacity() {
                let new_cap = match buffer.capacity() {
                    0 => 4096,
                    c => c * 2,
                };

                if new_cap > 100 * 1024 * 1024 {
                    return Err(anyhow::anyhow!("Buffer limit exceeded"));
                }

                let mut new_buf = allocator.allocate(new_cap);
                unsafe {
                    let len = buffer.len();
                    new_buf.set_len(len);
                    if len > 0 {
                        std::ptr::copy_nonoverlapping(
                            buffer.as_ptr(),
                            new_buf.as_mut_ptr(),
                            len,
                        );
                    }
                }
                allocator.release(buffer);
                buffer = new_buf;
            }

            let prev_len = buffer.len();
            let BufResult(res, returned) = reader.read(buffer).await;
            buffer = returned;

            let n = match res {
                Ok(0) => {
                    info!("Reader EOF");
                    break;
                }
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };

            unsafe { buffer.set_len(prev_len + n) };

            let mut offset = 0;
            loop {
                let available = buffer.len() - offset;

                if available < 4 {
                    break;
                }

                let msg_len = u32::from_le_bytes(
                    buffer.as_init()[offset..offset + 4].try_into().unwrap(),
                ) as usize;

                let total = 4 + msg_len;
                if available < total {
                    break;
                }

                let mut payload = allocator.allocate(msg_len);
                unsafe {
                    payload.set_len(msg_len);
                    std::ptr::copy_nonoverlapping(
                        buffer.as_ptr().add(offset + 4),
                        payload.as_mut_ptr(),
                        msg_len,
                    );
                }

                if incoming_tx.send(payload).await.is_err() {
                    return Err(anyhow::anyhow!("Incoming channel closed"));
                }

                offset += total;
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

        allocator.release(buffer);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use crate::transport::base::MessageSink;
use super::*;
    use crate::transport::impls::peer::config::PeerConfig;
    use crate::transport::impls::peer::handle::PeerSource;
    use crate::pool::pool_local::TpcStrategy;
    use crate::transport::stream::vsock::VsockTarget;
    use crate::transport::stream::vsock::general::{VListener, VStream};
    use tracing::debug;
    use crate::codecs::serde_protocol::VecBuf;
    use crate::pool::allocator::TpcAllocator;
    use crate::transport::base::pool_config::PoolConfig;

    async fn setup_vsock_pair(port: u32) -> (VStream, VStream) {
        let listener = VListener::bind_loopback(port).expect("Vsock bind failed");

        let server_handle = compio::runtime::spawn(async move {
            listener.accept().await.expect("Vsock accept failed")
        });

        let client_stream = VStream::connect(VsockTarget::Cid(0), port)
            .await
            .expect("Vsock connect failed");

        let (server_stream, _) = server_handle.await.unwrap();
        (server_stream, client_stream)
    }

    fn make_peer(
        stream: impl crate::transport::stream::AsyncStream + Clone + 'static,
    ) -> (
        crate::transport::impls::peer::handle::PeerSink<VecBuf>,
        PeerSource<VecBuf>,
    ) {
        let a: TpcAllocator<VecBuf> = BufferAllocator::get(&PoolConfig::default());
        Peer::new(stream, PeerConfig::default(), a).unwrap()
    }

    #[compio::test]
    async fn test_peer_simple_delivery() {
        let (server_stream, client_stream) = setup_vsock_pair(8000).await;

        let (mut s_tx, _) = make_peer(server_stream);
        let (_, PeerSource { incoming_rx: mut c_rx }) = make_peer(client_stream);

        let msg: VecBuf = vec![1u8, 3u8, 3u8, 7u8].into();
        s_tx.send(msg.clone()).await.unwrap();

        let received = c_rx.recv().await.expect("No message received");
        assert_eq!(received, msg);
    }

    #[compio::test]
    async fn test_peer_large_packet() {
        let (server_stream, client_stream) = setup_vsock_pair(8001).await;

        let (s_tx, _) = make_peer(server_stream);
        let (_, PeerSource { incoming_rx: mut c_rx }) = make_peer(client_stream);

        let large_data: VecBuf = vec![0xAAu8; 1024 * 1024].into();
        s_tx.send(large_data.clone()).await.unwrap();

        let received = c_rx.recv().await.expect("No message received");
        assert_eq!(received.len(), large_data.len());
        assert_eq!(received, large_data);
    }

    #[compio::test]
    async fn test_peer_full_duplex_bidirectional() {
        let (server_stream, client_stream) = setup_vsock_pair(8002).await;
        
        let (mut s_tx, PeerSource { incoming_rx: mut s_rx }) = make_peer(server_stream);
        let (mut c_tx, PeerSource { incoming_rx: mut c_rx }) = make_peer(client_stream);

        let mut s_data = vec![0x11u8; 64];
        let mut c_data = vec![0x22u8; 64];

        s_data[0..8].copy_from_slice(&0xDEADBEEF_00000001u64.to_le_bytes());
        s_data[56..64].copy_from_slice(&0xDEADBEEF_00000002u64.to_le_bytes());
        c_data[0..8].copy_from_slice(&0xDEADBEEF_00000001u64.to_le_bytes());
        c_data[56..64].copy_from_slice(&0xDEADBEEF_00000002u64.to_le_bytes());

        let (res_c, res_s) = futures::join!(
            async {
                c_tx.send(c_data.clone().into()).await.unwrap();
                c_rx.recv().await.unwrap()
            },
            async {
                s_tx.send(s_data.clone().into()).await.unwrap();
                s_rx.recv().await.unwrap()
            }
        );

        assert_eq!(res_c, s_data.into());
        assert_eq!(res_s, c_data.into());
    }

    #[compio::test]
    async fn test_peer_multiple_messages_order() {
        let (server_stream, client_stream) = setup_vsock_pair(8003).await;

        let (mut s_tx, _) = make_peer(server_stream);
        let (_, PeerSource { incoming_rx: mut c_rx }) = make_peer(client_stream);

        let counts = [10usize, 20, 30];

        for &len in &counts {
            s_tx.send(vec![len as u8; len].into()).await.unwrap();
        }

        for &len in &counts {
            let received = c_rx.recv().await.expect("Failed to receive");
            assert_eq!(received.len(), len);
            assert!(received.0.iter().all(|&b| b == len as u8));
        }
    }

    #[compio::test]
    async fn test_peer_zero_length_packet() {
        let (server_stream, client_stream) = setup_vsock_pair(8004).await;

        let (mut s_tx, _) = make_peer(server_stream);
        let (_, PeerSource { incoming_rx: mut c_rx }) = make_peer(client_stream);

        s_tx.send(vec![].into()).await.unwrap();
        s_tx.send(vec![1, 2, 3].into()).await.unwrap();

        let received = c_rx.recv().await.unwrap();
        if received.is_empty() {
            let second = c_rx.recv().await.unwrap();
            assert_eq!(second, vec![1, 2, 3].into());
        } else {
            assert_eq!(received, vec![1, 2, 3].into());
        }
    }

    #[compio::test]
    async fn test_peer_varying_sizes_exceeding_buffer() {
        let (server_stream, client_stream) = setup_vsock_pair(8005).await;

        let (s_tx, _) = make_peer(server_stream);
        let (_, PeerSource { incoming_rx: mut c_rx }) = make_peer(client_stream);

        let sizes = [100usize, 4000, 5000, 64 * 1024, 1024 * 1024];

        for &size in &sizes {
            let content: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            debug!("Sending message of size {}", size);
            s_tx.send(content.into()).await.unwrap();
        }

        for &size in &sizes {
            let received = c_rx.recv().await.expect("Failed to receive message");
            debug!("Received message of len {}", received.len());

            let expected: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            assert_eq!(received.len(), size, "Size mismatch for {}", size);
            assert_eq!(received, expected.into(), "Content mismatch for size {}", size);
        }
    }
}