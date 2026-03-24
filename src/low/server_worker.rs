use std::cell::RefCell;
use crate::codecs::base::BufferAllocator;
use crate::high::service_handler::ServiceHandler;
use crate::low::handshake::{
    ConnectionGate, ConnectionLease, HandshakeMode, authenticate_server, reject_connection,
};
use crate::low::main_loop::{SessionConfig, run_session};
use crate::low::runtime;
use crate::transport::base::{
    MessageSink, MessageSource, TopologyRegistry, Transport, TransportAcceptor,
    TransportPerWorkerBuilder,
};
use anyhow::Result;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use rustc_hash::FxHashMap;
use tracing::{error, info};

pub struct ServerWorker<C: SessionConfig, H: ServiceHandler<C::Codec>> {
    phantom: PhantomData<(C, H)>,
}

impl<C: SessionConfig, H: ServiceHandler<C::Codec>> ServerWorker<C, H> {
    pub async fn spawn<B, Sink, Source>(
        core_id: usize,
        builder: B,
        handler: H,
        registry: Option<Arc<dyn TopologyRegistry>>,
        tx_allocator: C::TxAlloc,
        connection_gate: ConnectionGate,
        handshake: HandshakeMode,
    ) -> Result<()>
    where
        B: TransportPerWorkerBuilder<Sink, Source>,
        Sink: MessageSink<Payload = <C::TxAlloc as BufferAllocator>::Payload>,
        Source: MessageSource<Payload = C::RxPayload>,
        C::TxAlloc: Send,
    {
        runtime::spawn_on(core_id, move || async move {
            let acceptor = match builder.bind(core_id, registry.as_ref()).await {
                Ok(a) => a,
                Err(e) => {
                    error!(core_id, error = %e, "Failed to bind listener");
                    return;
                }
            };

            info!(core_id, "Server worker listening");

            loop {
                let alloc = tx_allocator.clone();
                match acceptor.accept().await {
                    Ok(transport) => {
                        info!("Accepted new connection");
                        let h = handler.clone();

                        let (sink, mut source) = match transport.decompose() {
                            Ok(pair) => pair,
                            Err(e) => {
                                error!(core_id, error = %e, "Transport decompose error");
                                compio::time::sleep(Duration::from_millis(1000)).await;
                                continue;
                            }
                        };

                        let lease = match connection_gate.try_acquire() {
                            Some(lease) => lease,
                            None => {
                                let _ = reject_connection(
                                    &sink,
                                    &tx_allocator,
                                    "Connection rejected: server is in one-to-one mode",
                                )
                                .await;
                                info!(core_id, "Rejected connection due to one-to-one mode");
                                continue;
                            }
                        };

                        if let Err(e) = authenticate_server(
                            &sink,
                            &mut source,
                            &tx_allocator,
                            &handshake,
                        )
                        .await
                        {
                            error!(core_id, error = %e, "Server handshake failed");
                            drop(lease);
                            continue;
                        }

                        compio::runtime::spawn(async move {
                            let _lease: ConnectionLease = lease;
                            info!("run_session task started");
                            let pending = Rc::new(RefCell::new(FxHashMap::default()));
                            run_session::<C, _, _, _>(h, sink, source, pending, alloc).await;
                        })
                        .detach();
                    }
                    Err(e) => {
                        error!(core_id, error = %e, "Accept error");
                        compio::time::sleep(Duration::from_millis(1000)).await;
                    }
                }
            }
        }).await;
        Ok(())
    }
}
