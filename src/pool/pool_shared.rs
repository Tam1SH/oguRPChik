use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicU64, Ordering};
use object_pool::Pool;
use crate::codecs::base::OwnedBuf;
use crate::pool::base::PoolStrategy;
use crate::transport::base::pool_config::PoolConfig;

pub struct SharedStrategy<B: OwnedBuf> {
    pool: Arc<Pool<B>>,
    ema: Arc<AtomicU64>,
    threshold_factor: f32,
    ema_alpha: f32,
}

static SHARED_POOLS: OnceLock<Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>> =
    OnceLock::new();

fn global_pool<B: OwnedBuf>(initial_cap: usize) -> Arc<Pool<B>> {
    let pools = SHARED_POOLS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = pools.lock().unwrap();

    map.entry(TypeId::of::<B>())
        .or_insert_with(|| {
            Arc::new(Pool::<B>::new(0, || B::with_capacity(initial_cap)))
                as Arc<dyn Any + Send + Sync>
        })
        .clone()
        .downcast::<Pool<B>>()
        .unwrap()
}

impl<B: OwnedBuf> SharedStrategy<B> {
    pub fn new(initial_cap: usize, threshold_factor: f32, ema_alpha: f32) -> Self {
        Self {
            pool: Arc::new(Pool::new(0, || B::with_capacity(initial_cap))),
            ema: Arc::new(AtomicU64::new(initial_cap as u64)),
            threshold_factor,
            ema_alpha,
        }
    }

    fn update_ema(&self, cap: usize) -> usize {
        loop {
            let old = self.ema.load(Ordering::Relaxed);
            let new_ema = (old as f32 * (1.0 - self.ema_alpha)
                + cap as f32 * self.ema_alpha) as u64;
            if self
                .ema
                .compare_exchange_weak(old, new_ema, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return new_ema as usize;
            }
        }
    }
}

impl<B: OwnedBuf> Clone for SharedStrategy<B> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            ema: self.ema.clone(),
            threshold_factor: self.threshold_factor,
            ema_alpha: self.ema_alpha,
        }
    }
}

impl<B: OwnedBuf> PoolStrategy<B> for SharedStrategy<B> {
    type SendMark = ();

    fn get(config: &PoolConfig) -> Self {
        Self {
            pool: global_pool::<B>(config.initial_cap),
            ema: Arc::new(AtomicU64::new(config.ema_alpha as u64)),
            threshold_factor: config.threshold_factor,
            ema_alpha: config.ema_alpha,
        }
    }

    fn acquire(&self, cap: usize) -> B {
        let mut buf = self.pool.pull(|| B::with_capacity(cap)).detach().1;
        if buf.capacity() < cap {
            buf = B::with_capacity(cap);
        }
        buf.clear();
        buf
    }

    fn release(&self, mut buf: B) {
        let cap = buf.capacity();
        let ema = self.update_ema(cap);

        if cap > (ema as f32 * self.threshold_factor) as usize && cap > 131072 {
            return;
        }

        buf.clear();
        self.pool.attach(buf);
    }
}