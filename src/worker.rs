use crate::main_loop::{run_session, SessionConfig};
use crate::message_codec::{HandshakeCodec, MessageCodec};
use crate::runtime;
use crate::transport::stream::AcceptorBuilder;
use anyhow::{Context, Result};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use dashmap::DashMap;
use tracing::{error, info};
use crate::service_handler::ServiceHandler;
use crate::transport::base::{BufferAllocator, MessageSink, MessageSource, TopologyRegistry, Transport, TransportAcceptor, TransportPerWorkerBuilder, WorkerInitializer};
use crate::transport::discovery::Topology;

pub struct ServerWorker<C: SessionConfig, H: ServiceHandler<C::Codec>> {
    phantom: PhantomData<(C, H)>,
}


impl<C: SessionConfig + 'static, H: ServiceHandler<C::Codec> + Clone + Send + 'static> ServerWorker<C, H> {
    pub fn spawn<B, Sink, Source>(
        core_id: usize,
        builder: B,
        handler: H,
        registry: Option<Arc<dyn TopologyRegistry>>,
        peer_tx: Option<flume::Sender<Topology>>,
        topology: Option<Topology>,
    ) -> Result<()>
    where
        B: TransportPerWorkerBuilder<Sink, Source> + Send + 'static,
        Sink: MessageSink<Payload = C::Payload> + 'static,
        Source: MessageSource<Payload = C::Payload> + 'static,
    {
        runtime::spawn_on(core_id, move || async move {
            let acceptor = match builder.bind(core_id, registry.as_ref()).await {
                Ok(a) => a,
                Err(e) => {
                    error!(core_id, error = %e, "Failed to bind listener");
                    return;
                }
            };
            
            B::Initializer::init(core_id);
            
            info!(core_id, "Server worker listening");

            loop {
                let topology = topology.clone();
                match acceptor.accept().await {
                    Ok(transport) => {
                        info!("accepted new connection");
                        let h = handler.clone();

                        let (sink, mut source) = match transport.decompose() {
                            Ok(transport) => transport,
                            Err(e) => {
                                error!(core_id, error = %e, "Transport error");
                                compio::time::sleep(Duration::from_millis(1000)).await;
                                continue
                            }
                        };

                        let peer_tx = peer_tx.clone();
                        compio::runtime::spawn(async move {
                            info!("run_session task started");

                            let peer_topology = match source.recv().await {
                                Some(raw) => match <<C as SessionConfig>::Codec as MessageCodec>::Handshake::decode_handshake(raw.as_ref()) {
                                    Ok(t) => t,
                                    Err(e) => { error!("Handshake decode error: {e}"); return; }
                                },
                                None => { error!("Connection closed during handshake"); return; }
                            };

                            let mut buf = Default::default();
                            
                            if let Some(topology) = topology {
                                match <<C as SessionConfig>::Codec as MessageCodec>::Handshake::encode_handshake(&topology, &mut buf) {
                                    Ok(_) => { let _ = sink.send(buf).await; }
                                    Err(e) => { error!("Handshake encode error: {e}"); return; }
                                }
                            }
                            
                            if let Some(tx) = &peer_tx {
                                let _ = tx.send_async(peer_topology).await;
                            }

                            let pending = Rc::new(DashMap::new());

                            run_session::<C, _, _, _>(h, sink, source, pending).await;
                        })
                            .detach();
                    }
                    Err(e) => {
                        error!(core_id, error = %e, "Accept error");
                        compio::time::sleep(Duration::from_millis(1000)).await;
                    }
                }
            }
        });
        Ok(())
    }
}
