use crate::main_loop::{run_session, SessionConfig};
use crate::message_codec::MessageCodec;
use crate::runtime;
use crate::transport::stream::AcceptorBuilder;
use crate::ServiceHandler;
use anyhow::{Context, Result};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use dashmap::DashMap;
use tracing::{error, info};
use crate::transport::base::{BufferAllocator, MessageSink, MessageSource, TopologyRegistry, Transport, TransportAcceptor, TransportPerWorkerBuilder, WorkerInitializer};

pub struct ServerWorker<C: SessionConfig, H: ServiceHandler<C::Codec>> {
    phantom: PhantomData<(C, H)>,
}


impl<C: SessionConfig + 'static, H: ServiceHandler<C::Codec> + Clone + Send + 'static> ServerWorker<C, H> {
    pub fn spawn<B, Sink, Source>(
        core_id: usize,
        builder: B,
        handler: H,
        registry: Option<Arc<dyn TopologyRegistry>>,
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
                match acceptor.accept().await {
                    Ok(transport) => {
                        let h = handler.clone();

                        let (sink, source) = match transport.decompose() {
                            Ok(transport) => transport,
                            Err(e) => {
                                error!(core_id, error = %e, "Transport error");
                                compio::time::sleep(Duration::from_millis(1000)).await;
                                continue
                            }
                        };

                        let pending = Rc::new(DashMap::new());

                        compio::runtime::spawn(async move {
                            run_session::<C, _, _, _>(h, sink, source, pending).await
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
