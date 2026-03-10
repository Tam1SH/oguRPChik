#[derive(Clone, Debug)]
pub struct PoolConfig {
    pub initial_cap: usize,
    pub threshold_factor: f32,
    pub ema_alpha: f32,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            initial_cap: 4096,
            threshold_factor: 3.0,
            ema_alpha: 0.1,
        }
    }
}
