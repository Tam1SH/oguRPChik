use crate::codecs::base::OwnedBuf;
use crate::pool::pool_stats::PoolStats;
use object_pool::Pool;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

#[derive(Clone)]
pub struct PoolCore {
    pub threshold_factor: f32,
    pub ema_alpha: f32,
    pub hard_drop_bytes: usize,
    pub stats: Arc<PoolStats>,
}

impl PoolCore {
    pub fn new(threshold_factor: f32, ema_alpha: f32, hard_drop_bytes: usize) -> Self {
        Self {
            threshold_factor,
            ema_alpha,
            hard_drop_bytes,
            stats: Arc::new(PoolStats::default()),
        }
    }

    #[inline]
    pub fn effective_cap(&self, requested: usize, ema: f32) -> usize {
        requested.max(ema as usize)
    }

    #[inline]
    pub fn next_ema(&self, current: f32, used: usize) -> f32 {
        current * (1.0 - self.ema_alpha) + used as f32 * self.ema_alpha
    }

    #[inline]
    pub fn should_drop(&self, cap: usize, ema: f32) -> bool {
        cap > (ema * self.threshold_factor) as usize || cap > self.hard_drop_bytes
    }

    pub fn do_acquire<B: OwnedBuf>(&self, size_hint: usize, ema: f32, pool: &Pool<B>) -> B {
        self.stats.acquire_total.fetch_add(1, Relaxed);
        self.stats
            .pool_size_snapshot
            .store(pool.len() as u32, Relaxed);

        let effective = self.effective_cap(size_hint, ema);

        match pool.try_pull() {
            None => {
                self.stats.acquire_misses.fetch_add(1, Relaxed);
                let mut b = B::with_capacity(effective);
                b.clear();
                b
            }
            Some(pulled) => {
                self.stats.acquire_hits.fetch_add(1, Relaxed);
                let mut buf = pulled.detach().1;

                if buf.capacity() < effective {
                    self.stats.acquire_reallocs.fetch_add(1, Relaxed);
                    buf = B::with_capacity(effective);
                }

                buf.clear();
                buf
            }
        }
    }

    pub fn do_release<B: OwnedBuf>(&self, mut buf: B, ema: f32, pool: &Pool<B>) -> f32 {
        let used = buf.len();
        let cap = buf.capacity();

        self.stats.release_total.fetch_add(1, Relaxed);
        self.stats.last_used_bytes.store(used as u64, Relaxed);

        let new_ema = self.next_ema(ema, used);
        self.stats
            .ema_snapshot_x100
            .store((new_ema * 100.0) as u32, Relaxed);

        if self.should_drop(cap, new_ema) {
            self.stats.release_dropped.fetch_add(1, Relaxed);
            return new_ema;
        }

        buf.clear();
        self.stats.release_returned.fetch_add(1, Relaxed);
        pool.attach(buf);
        new_ema
    }
}
