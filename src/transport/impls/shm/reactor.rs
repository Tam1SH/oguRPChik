use dashmap::DashMap;
use iceoryx2::port::listener::Listener;
use iceoryx2::prelude::*;
use iceoryx2::service::ipc_threadsafe::Service;
use iceoryx2::waitset::{WaitSet, WaitSetAttachmentId, WaitSetGuard};
use std::sync::Arc;
use std::task::Waker;
use tracing::{debug, error, trace};

pub enum ReactorCmd {
    Attach(
        Listener<Service>,
        flume::Sender<WaitSetAttachmentId<Service>>,
    ),
}

unsafe impl Send for ReactorCmd {}

pub struct GlobalReactor {
    wakers: Arc<DashMap<WaitSetAttachmentId<Service>, Waker>>,
    cmd_tx: flume::Sender<ReactorCmd>,
}

lazy_static::lazy_static! {
    static ref REACTOR: Arc<GlobalReactor> = GlobalReactor::start();
}

impl GlobalReactor {
    pub fn get() -> Arc<Self> {
        REACTOR.clone()
    }

    fn start() -> Arc<Self> {
        let (cmd_tx, cmd_rx) = flume::unbounded::<ReactorCmd>();
        let wakers = Arc::new(DashMap::<WaitSetAttachmentId<Service>, Waker>::new());
        let wakers_inner = wakers.clone();

        std::thread::spawn(move || {
            let waitset = WaitSetBuilder::new()
                .create::<Service>()
                .expect("Failed to create Global WaitSet");

            let mut attachments = Vec::new();

            loop {
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        ReactorCmd::Attach(listener, reply_tx) => {
                            let boxed = Box::new(listener);

                            let listener_ref = unsafe { &*(boxed.as_ref() as *const _) };

                            match waitset.attach_notification(listener_ref) {
                                Ok(guard) => {
                                    let id = WaitSetAttachmentId::from_guard(&guard);
                                    let guard_static: WaitSetGuard<'static, 'static, Service> =
                                        unsafe { std::mem::transmute(guard) };
                                    attachments.push((boxed, guard_static));
                                    let _ = reply_tx.send(id);
                                }
                                Err(e) => error!("Global Reactor Attach error: {:?}", e),
                            }
                        }
                    }
                }

                let _ = waitset.wait_and_process_once_with_timeout(
                    |id| {
                        if let Some((_, waker)) = wakers_inner.remove(&id) {
                            waker.wake();
                        }
                        CallbackProgression::Continue
                    },
                    std::time::Duration::from_micros(50),
                );
            }
        });

        Arc::new(Self { wakers, cmd_tx })
    }

    pub fn register(&self, id: WaitSetAttachmentId<Service>, waker: Waker) {
        self.wakers.insert(id, waker);
    }

    pub fn unregister(&self, id: &WaitSetAttachmentId<Service>) {
        self.wakers.remove(id);
    }
    pub fn attach(&self, listener: Listener<Service>) -> WaitSetAttachmentId<Service> {
        let (tx, rx) = flume::bounded(1);
        self.cmd_tx.send(ReactorCmd::Attach(listener, tx)).unwrap();
        rx.recv().expect("Global Reactor died")
    }
}
