#[cfg(test)]
mod pool_convergence_tests {

    #[derive(Clone, Default)]
    struct FakeBuf {
        data: Vec<u8>,
        used_len: usize,
    }

    impl AsRef<[u8]> for FakeBuf {
        fn as_ref(&self) -> &[u8] {
            &self.data[..self.used_len]
        }
    }

    impl crate::codecs::base::OwnedBuf for FakeBuf {
        fn with_capacity(capacity: usize) -> Self {
            Self {
                data: vec![0u8; capacity],
                used_len: 0,
            }
        }
        fn capacity(&self) -> usize {
            self.data.len()
        }
        fn len(&self) -> usize {
            self.used_len
        }
        fn as_ptr(&self) -> *const u8 {
            self.data.as_ptr()
        }
        fn as_mut_ptr(&mut self) -> *mut u8 {
            self.data.as_mut_ptr()
        }
        fn clear(&mut self) {
            self.used_len = 0;
        }
    }

    impl FakeBuf {
        fn write(&mut self, n: usize) {
            assert!(
                n <= self.capacity(),
                "encode overflow: {n} > {}",
                self.capacity()
            );
            self.used_len = n;
        }
    }

    use crate::codecs::base::OwnedBuf;
    use crate::pool::base::PoolStrategy;
    use crate::pool::pool_local::TpcStrategy;

    fn simulate_n<F>(strategy: &TpcStrategy<FakeBuf>, initial_hint: usize, n: usize, msg_size_fn: F)
    where
        F: Fn(usize) -> usize,
    {
        for i in 0..n {
            let mut buf = strategy.acquire(initial_hint);
            let msg_size = msg_size_fn(i);
            if msg_size > buf.capacity() {
                buf = FakeBuf::with_capacity(msg_size);
            }
            buf.write(msg_size);
            strategy.release(buf);
        }
    }

    #[test]
    fn test_ema_converges_constant_message_size() {
        const MSG_SIZE: usize = 216;
        const WARMUP: usize = 30;
        const MEASURE: usize = 100;

        let strategy = TpcStrategy::<FakeBuf>::new(64, 4.0, 0.15, 131_072);

        simulate_n(&strategy, MSG_SIZE, WARMUP, |_| MSG_SIZE);
        let after_warmup = strategy.stats().snapshot();

        simulate_n(&strategy, MSG_SIZE, MEASURE, |_| MSG_SIZE);
        let after_measure = strategy.stats().snapshot();

        let delta_hits = after_measure.acquire_hits - after_warmup.acquire_hits;
        let delta_reallocs = after_measure.acquire_reallocs - after_warmup.acquire_reallocs;
        let delta_total = after_measure.acquire_total - after_warmup.acquire_total;

        let hit_rate_pct = delta_hits as f64 / delta_total as f64 * 100.0;
        let realloc_pct = delta_reallocs as f64 / delta_hits.max(1) as f64 * 100.0;
        let ema_bytes = after_measure.ema_bytes;

        println!("=== Constant MSG ({MSG_SIZE}B) ===");
        println!("  hit_rate:    {hit_rate_pct:.1}%  (expected >= 99%)");
        println!("  realloc_pct: {realloc_pct:.1}%   (expected ~0%)");
        println!("  ema_bytes:   {ema_bytes:.1}B     (expected ~{MSG_SIZE})");
        println!("  full: {after_measure}");

        assert!(
            hit_rate_pct >= 99.0,
            "hit_rate {hit_rate_pct:.1}% < 99% after warmup"
        );
        assert!(
            realloc_pct < 1.0,
            "realloc_of_hit {realloc_pct:.1}% >= 1% -- EMA has not converged"
        );

        let ema_error_pct = (ema_bytes - MSG_SIZE as f64).abs() / MSG_SIZE as f64 * 100.0;
        assert!(
            ema_error_pct < 20.0,
            "EMA {ema_bytes:.1}B is too far from {MSG_SIZE}B (error {ema_error_pct:.1}%)"
        );
    }

    #[test]
    fn test_ema_corrects_oversized_hint() {
        const MSG_SIZE: usize = 216;
        const HINT: usize = MSG_SIZE * 2;
        const WARMUP: usize = 30;
        const MEASURE: usize = 100;

        let strategy = TpcStrategy::<FakeBuf>::new(HINT, 4.0, 0.15, 131_072);

        simulate_n(&strategy, HINT, WARMUP, |_| MSG_SIZE);
        let after_warmup = strategy.stats().snapshot();

        simulate_n(&strategy, HINT, MEASURE, |_| MSG_SIZE);
        let after_measure = strategy.stats().snapshot();

        let delta_hits = after_measure.acquire_hits - after_warmup.acquire_hits;
        let delta_reallocs = after_measure.acquire_reallocs - after_warmup.acquire_reallocs;
        let delta_total = after_measure.acquire_total - after_warmup.acquire_total;

        let hit_rate_pct = delta_hits as f64 / delta_total as f64 * 100.0;
        let realloc_pct = delta_reallocs as f64 / delta_hits.max(1) as f64 * 100.0;

        println!("=== Oversized hint ({HINT}B, real msg {MSG_SIZE}B) ===");
        println!("  hit_rate:    {hit_rate_pct:.1}%");
        println!("  realloc_pct: {realloc_pct:.1}%");
        println!("  ema_bytes:   {:.1}B", after_measure.ema_bytes);

        assert!(hit_rate_pct >= 99.0, "hit_rate {hit_rate_pct:.1}% < 99%");
        assert_eq!(
            delta_reallocs, 0,
            "realloc must be 0 with oversized hint -- buf is always large enough"
        );
    }

    #[test]
    fn test_ema_bimodal_no_realloc_after_warmup() {
        const SMALL: usize = 200;
        const LARGE: usize = 300;
        const WARMUP: usize = 50;
        const MEASURE: usize = 200;

        let strategy = TpcStrategy::<FakeBuf>::new(256, 4.0, 0.15, 131_072);
        let bimodal = |i: usize| if i % 2 == 0 { SMALL } else { LARGE };

        simulate_n(&strategy, LARGE, WARMUP, bimodal);
        let after_warmup = strategy.stats().snapshot();

        simulate_n(&strategy, LARGE, MEASURE, bimodal);
        let after_measure = strategy.stats().snapshot();

        let delta_hits = after_measure.acquire_hits - after_warmup.acquire_hits;
        let delta_reallocs = after_measure.acquire_reallocs - after_warmup.acquire_reallocs;
        let delta_total = after_measure.acquire_total - after_warmup.acquire_total;

        let hit_rate_pct = delta_hits as f64 / delta_total as f64 * 100.0;
        let realloc_pct = delta_reallocs as f64 / delta_hits.max(1) as f64 * 100.0;

        println!("=== Bimodal ({SMALL}B / {LARGE}B) ===");
        println!("  hit_rate:    {hit_rate_pct:.1}%");
        println!("  realloc_pct: {realloc_pct:.1}%");
        println!(
            "  ema_bytes:   {:.1}B  (expected ~250B)",
            after_measure.ema_bytes
        );

        assert!(hit_rate_pct >= 99.0, "hit_rate {hit_rate_pct:.1}% < 99%");
        assert!(
            realloc_pct < 1.0,
            "realloc {realloc_pct:.1}% >= 1% after warmup"
        );
    }

    #[test]
    fn test_oversized_buffer_dropped() {
        const NORMAL_SIZE: usize = 216;
        const SPIKE_SIZE: usize = 200_000;

        let strategy = TpcStrategy::<FakeBuf>::new(256, 4.0, 0.15, 131_072);

        simulate_n(&strategy, NORMAL_SIZE, 20, |_| NORMAL_SIZE);

        let mut spike = FakeBuf::with_capacity(SPIKE_SIZE);
        spike.write(SPIKE_SIZE);
        strategy.release(spike);

        let snap = strategy.stats().snapshot();
        println!("=== Spike drop test ===");
        println!("  {snap}");

        assert!(snap.release_dropped >= 1, "spike buffer should be dropped");
    }
}
