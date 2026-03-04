use crate::client::Priority;

#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub priority: Priority,
    pub channel_size: usize,
    pub batch_limit: usize,
    pub read_buffer_capacity: usize,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self::for_priority(Priority::Normal)
    }
}

impl PeerConfig {
    pub fn for_priority(priority: Priority) -> Self {
        match priority {
            Priority::Critical => Self {
                priority,
                channel_size: 16,
                batch_limit: 16,
                read_buffer_capacity: 32 * 1024,
            },
            Priority::Normal => Self {
                priority,
                channel_size: 16,
                batch_limit: 32,
                read_buffer_capacity: 256 * 1024,
            },
            Priority::Bulk => Self {
                priority,
                channel_size: 32,
                batch_limit: 64,
                read_buffer_capacity: 1024 * 1024,
            },
        }
    }
}
