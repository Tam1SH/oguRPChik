#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub channel_size: usize,
    pub batch_limit: usize,
    pub read_buffer_capacity: usize,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            channel_size: 16,
            batch_limit: 32,
            read_buffer_capacity: 256 * 1024,
        }
    }
}
