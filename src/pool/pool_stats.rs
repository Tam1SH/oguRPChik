use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub struct PoolStats {
    pub acquire_total: AtomicU64,
    pub acquire_hits: AtomicU64,
    pub acquire_misses: AtomicU64,
    pub acquire_reallocs: AtomicU64,

    pub release_total: AtomicU64,
    pub release_returned: AtomicU64,
    pub release_dropped: AtomicU64,

    pub ema_snapshot_x100: AtomicU32,
    pub last_used_bytes: AtomicU64,

    pub pool_size_snapshot: AtomicU32,
}

impl Default for PoolStats {
    fn default() -> Self {
        Self {
            acquire_total: AtomicU64::new(0),
            acquire_hits: AtomicU64::new(0),
            acquire_misses: AtomicU64::new(0),
            acquire_reallocs: AtomicU64::new(0),
            release_total: AtomicU64::new(0),
            release_returned: AtomicU64::new(0),
            release_dropped: AtomicU64::new(0),
            ema_snapshot_x100: AtomicU32::new(0),
            last_used_bytes: AtomicU64::new(0),
            pool_size_snapshot: AtomicU32::new(0),
        }
    }
}

impl PoolStats {
    pub fn snapshot(&self) -> PoolStatsSnapshot {
        let acquire_total = self.acquire_total.load(Ordering::Relaxed);
        let acquire_hits = self.acquire_hits.load(Ordering::Relaxed);
        let acquire_misses = self.acquire_misses.load(Ordering::Relaxed);
        let acquire_reallocs = self.acquire_reallocs.load(Ordering::Relaxed);

        let release_total = self.release_total.load(Ordering::Relaxed);
        let release_returned = self.release_returned.load(Ordering::Relaxed);
        let release_dropped = self.release_dropped.load(Ordering::Relaxed);

        let hit_rate = if acquire_total > 0 {
            acquire_hits as f64 / acquire_total as f64 * 100.0
        } else {
            0.0
        };

        let realloc_rate = if acquire_hits > 0 {
            acquire_reallocs as f64 / acquire_hits as f64 * 100.0
        } else {
            0.0
        };

        let drop_rate = if release_total > 0 {
            release_dropped as f64 / release_total as f64 * 100.0
        } else {
            0.0
        };

        PoolStatsSnapshot {
            acquire_total,
            acquire_hits,
            acquire_misses,
            acquire_reallocs,
            release_total,
            release_returned,
            release_dropped,
            ema_bytes: self.ema_snapshot_x100.load(Ordering::Relaxed) as f64 / 100.0,
            last_used_bytes: self.last_used_bytes.load(Ordering::Relaxed),
            pool_size_snapshot: self.pool_size_snapshot.load(Ordering::Relaxed),
            hit_rate_pct: hit_rate,
            realloc_rate_pct: realloc_rate,
            drop_rate_pct: drop_rate,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PoolStatsSnapshot {
    pub acquire_total: u64,
    pub acquire_hits: u64,
    pub acquire_misses: u64,
    pub acquire_reallocs: u64,

    pub release_total: u64,
    pub release_returned: u64,
    pub release_dropped: u64,

    pub ema_bytes: f64,
    pub last_used_bytes: u64,
    pub pool_size_snapshot: u32,

    pub hit_rate_pct: f64,
    pub realloc_rate_pct: f64,
    pub drop_rate_pct: f64,
}

impl std::fmt::Display for PoolStatsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pool [ hit={:.1}% realloc={:.1}% drop={:.1}% | \
             acq={} hit={} miss={} realloc={} | \
             rel={} ret={} drop={} | \
             ema={:.0}B last={:.0}B pool_sz={} ]",
            self.hit_rate_pct,
            self.realloc_rate_pct,
            self.drop_rate_pct,
            self.acquire_total,
            self.acquire_hits,
            self.acquire_misses,
            self.acquire_reallocs,
            self.release_total,
            self.release_returned,
            self.release_dropped,
            self.ema_bytes,
            self.last_used_bytes as f64,
            self.pool_size_snapshot,
        )
    }
}
