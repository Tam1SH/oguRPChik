#[derive(Clone, Debug)]
pub struct PoolConfig {
    pub hard_drop_bytes: usize,
    pub initial_cap: usize,
    pub threshold_factor: f32,
    pub ema_alpha: f32,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            hard_drop_bytes: 1024 * 1024 * 1, // 1 MB
            threshold_factor: 3.0,
            initial_cap: 4096,
            ema_alpha: 0.1,
        }
    }
}
