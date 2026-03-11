use crate::codecs::base::OwnedBuf;
use crate::pool::base::PoolStrategy;
use crate::pool::pool_core::PoolCore;
use crate::pool::pool_stats::PoolStats;
use crate::transport::base::pool_config::PoolConfig;
use object_pool::Pool;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

static SHARED_POOLS: OnceLock<Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>> = OnceLock::new();

fn global_pool<B: OwnedBuf>(initial_cap: usize) -> Arc<Pool<B>> {
    SHARED_POOLS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(TypeId::of::<B>())
        .or_insert_with(|| {
            Arc::new(Pool::<B>::new(0, || B::with_capacity(initial_cap)))
                as Arc<dyn Any + Send + Sync>
        })
        .clone()
        .downcast::<Pool<B>>()
        .unwrap()
}

pub struct SharedStrategy<B: OwnedBuf> {
    pool: Arc<Pool<B>>,
    ema: Arc<AtomicU64>,
    core: PoolCore,
}

impl<B: OwnedBuf> SharedStrategy<B> {
    pub fn new(
        initial_cap: usize,
        threshold_factor: f32,
        ema_alpha: f32,
        hard_drop_bytes: usize,
    ) -> Self {
        Self {
            pool: Arc::new(Pool::new(0, || B::with_capacity(initial_cap))),
            ema: Arc::new(AtomicU64::new(initial_cap as u64)),
            core: PoolCore::new(threshold_factor, ema_alpha, hard_drop_bytes),
        }
    }

    pub fn stats(&self) -> Arc<PoolStats> {
        Arc::clone(&self.core.stats)
    }

    fn load_ema(&self) -> f32 {
        self.ema.load(Ordering::Relaxed) as f32
    }

    fn cas_ema(&self, expected_f32: f32, new_f32: f32) {
        let expected = expected_f32 as u64;
        let new = new_f32 as u64;

        let _ = self
            .ema
            .compare_exchange(expected, new, Ordering::Relaxed, Ordering::Relaxed);
    }
}

impl<B: OwnedBuf> Clone for SharedStrategy<B> {
    fn clone(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            ema: Arc::clone(&self.ema),
            core: self.core.clone(),
        }
    }
}

impl<B: OwnedBuf> PoolStrategy<B> for SharedStrategy<B> {
    type SendMark = (); // Send — backed by Arc<Pool>

    fn get(config: &PoolConfig) -> Self {
        Self {
            pool: global_pool::<B>(config.initial_cap),
            ema: Arc::new(AtomicU64::new(config.initial_cap as u64)),
            core: PoolCore::new(
                config.threshold_factor,
                config.ema_alpha,
                config.hard_drop_bytes,
            ),
        }
    }

    fn acquire(&self, size_hint: usize) -> B {
        self.core.do_acquire(size_hint, self.load_ema(), &self.pool)
    }

    fn release(&self, buf: B) {
        let old_ema = self.load_ema();
        let new_ema = self.core.do_release(buf, old_ema, &self.pool);
        self.cas_ema(old_ema, new_ema);
    }
}
