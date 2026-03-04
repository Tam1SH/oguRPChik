use crate::main_loop::{SessionConfig, run_session};
use crate::runtime;
use crate::service_handler::ServiceHandler;
use crate::transport::base::{
    MessageSink, MessageSource, TopologyRegistry, Transport, TransportAcceptor,
    TransportPerWorkerBuilder, WorkerInitializer,
};
use anyhow::Result;
use dashmap::DashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

pub struct ServerWorker<C: SessionConfig, H: ServiceHandler<C::Codec>> {
    phantom: PhantomData<(C, H)>,
}

impl<C: SessionConfig + 'static, H: ServiceHandler<C::Codec> + Clone + Send + 'static>
    ServerWorker<C, H>
{
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
                        info!("accepted new connection");
                        let h = handler.clone();

                        let (sink, source) = match transport.decompose() {
                            Ok(transport) => transport,
                            Err(e) => {
                                error!(core_id, error = %e, "Transport error");
                                compio::time::sleep(Duration::from_millis(1000)).await;
                                continue;
                            }
                        };

                        compio::runtime::spawn(async move {
                            info!("run_session task started");

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
