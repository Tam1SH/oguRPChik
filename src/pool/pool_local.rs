use crate::codecs::base::OwnedBuf;
use crate::pool::base::PoolStrategy;
use crate::pool::pool_core::PoolCore;
use crate::pool::pool_stats::PoolStats;
use crate::transport::base::pool_config::PoolConfig;
use object_pool::Pool;
use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::Arc;

thread_local! {
    static TL_STATE: std::cell::RefCell<anymap::AnyMap> =
        std::cell::RefCell::new(anymap::AnyMap::new());
}

pub struct TpcStrategy<B: OwnedBuf> {
    initial_cap: usize,
    core: PoolCore,
    _marker: PhantomData<B>,
}

impl<B: OwnedBuf> TpcStrategy<B> {
    pub fn new(
        initial_cap: usize,
        threshold_factor: f32,
        ema_alpha: f32,
        hard_drop_bytes: usize,
    ) -> Self {
        Self {
            initial_cap,
            core: PoolCore::new(threshold_factor, ema_alpha, hard_drop_bytes),
            _marker: PhantomData,
        }
    }

    pub fn stats(&self) -> Arc<PoolStats> {
        Arc::clone(&self.core.stats)
    }

    fn with_tl<R>(&self, f: impl FnOnce(&Pool<B>, &Cell<f32>) -> R) -> R {
        TL_STATE.with(|state| {
            let mut map = state.borrow_mut();

            if map.get::<Pool<B>>().is_none() {
                map.insert(Pool::<B>::new(0, || B::with_capacity(self.initial_cap)));
                map.insert(Cell::new(self.initial_cap as f32));
            }

            f(
                map.get::<Pool<B>>().unwrap(),
                map.get::<Cell<f32>>().unwrap(),
            )
        })
    }
}

impl<B: OwnedBuf> Clone for TpcStrategy<B> {
    fn clone(&self) -> Self {
        Self {
            initial_cap: self.initial_cap,
            core: self.core.clone(),
            _marker: PhantomData,
        }
    }
}

impl<B: OwnedBuf> PoolStrategy<B> for TpcStrategy<B> {
    type SendMark = *const (); // !Send — backed by TLS

    fn get(config: &PoolConfig) -> Self {
        Self {
            initial_cap: config.initial_cap,
            core: PoolCore::new(
                config.threshold_factor,
                config.ema_alpha,
                config.hard_drop_bytes,
            ),
            _marker: PhantomData,
        }
    }

    fn acquire(&self, size_hint: usize) -> B {
        self.with_tl(|pool, ema| self.core.do_acquire(size_hint, ema.get(), pool))
    }

    fn release(&self, buf: B) {
        self.with_tl(|pool, ema| {
            let new_ema = self.core.do_release(buf, ema.get(), pool);
            ema.set(new_ema);
        });
    }
}
