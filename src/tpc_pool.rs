use crate::align_buffer::AlignedBuffer;
use bytes::{BufMut, BytesMut};
use compio::buf::{IoBuf, IoBufMut, SetLen};
use rkyv::util::AlignedVec;
use std::cell::RefCell;
use std::mem::MaybeUninit;

const BUCKETS: [usize; 13] = [
    8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 16384, 65536, 131072,
];

thread_local! {
    static POOL: RefCell<InnerPool> = RefCell::new(InnerPool::new(PoolConfig::default()));
}

pub struct TpcPool;

impl TpcPool {

    pub fn init(config: PoolConfig) {
        POOL.with(|p| {
            *p.borrow_mut() = InnerPool::new(config);
        });
    }

    pub fn with<F, R>(f: F) -> R
    where
        F: FnOnce(&mut InnerPool) -> R,
    {
        POOL.with(|p| f(&mut p.borrow_mut()))
    }

    #[inline]
    pub fn acquire_header() -> BytesMut {
        Self::with(|p| p.acquire_header_raw(0))
    }

    #[inline]
    pub fn release_header(buf: BytesMut) {
        Self::with(|p| p.release_header(buf))
    }

    #[inline]
    pub fn acquire_body(needed_cap: usize) -> AlignedBuffer {
        Self::with(|p| p.acquire_body(needed_cap))
    }

    #[inline]
    pub fn release_body(buf: AlignedBuffer) {
        Self::with(|p| p.release_body(buf))
    }
}

pub struct BucketConfig {
    pub size: usize,
    pub max_count: usize,
}

pub struct PoolConfig {
    pub header_initial_cap: usize,
    pub header_max_count: usize,
    pub buckets: Vec<BucketConfig>,
    pub ema_alpha: f32,
    pub ema_threshold_factor: f32,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self::standard()
    }
}

impl PoolConfig {

    pub fn light() -> Self {
        Self {
            header_max_count: 64,
            header_initial_cap: 4,
            ema_alpha: 0.1,
            ema_threshold_factor: 2.0,
            buckets: vec![
                BucketConfig { size: 64, max_count: 64 },
                BucketConfig { size: 512, max_count: 64 },
                BucketConfig { size: 1024, max_count: 64 },
                BucketConfig { size: 4096, max_count: 32 },
                BucketConfig { size: 16384, max_count: 16 },
                BucketConfig { size: 65536, max_count: 8 },
            ],
        }
    }

    pub fn standard() -> Self {
        Self {
            header_max_count: 128,
            header_initial_cap: 4,
            ema_alpha: 0.1,
            ema_threshold_factor: 3.0,
            buckets: vec![
                BucketConfig { size: 128, max_count: 128 },
                BucketConfig { size: 512, max_count: 128 },
                BucketConfig { size: 1024, max_count: 128 },
                BucketConfig { size: 4096, max_count: 64 },
                BucketConfig { size: 16384, max_count: 64 },
                BucketConfig { size: 65536, max_count: 32 },
                BucketConfig { size: 131072, max_count: 16 },
            ],
        }
    }

    pub fn stress() -> Self {
        Self {
            header_max_count: 1024,
            header_initial_cap: 8,
            ema_alpha: 0.05,
            ema_threshold_factor: 5.0,
            buckets: vec![
                BucketConfig { size: 1024, max_count: 1024 },
                BucketConfig { size: 8192, max_count: 512 },
                BucketConfig { size: 65536, max_count: 256 },
                BucketConfig { size: 262144, max_count: 128 },
                BucketConfig { size: 524288, max_count: 64 },
                BucketConfig { size: 1048576, max_count: 32 },
            ],
        }
    }
}

pub struct InnerPool {
    config: PoolConfig,
    headers: Vec<BytesMut>,
    body_buckets: Vec<Vec<AlignedBuffer>>,
    ema_size: usize,
}

impl InnerPool {
    pub fn new(config: PoolConfig) -> Self {
        let mut headers = Vec::with_capacity(config.header_max_count);
        for _ in 0..config.header_max_count {
            headers.push(BytesMut::with_capacity(config.header_initial_cap));
        }

        let mut body_buckets = Vec::with_capacity(config.buckets.len());
        for b_conf in &config.buckets {
            let mut bucket = Vec::with_capacity(b_conf.max_count);
            for _ in 0..b_conf.max_count {
                bucket.push(AlignedBuffer(AlignedVec::with_capacity(b_conf.size)));
            }
            body_buckets.push(bucket);
        }

        InnerPool {
            ema_size: config.buckets.last().map(|b| b.size).unwrap_or(65536),
            headers,
            body_buckets,
            config,
        }
    }

    pub fn acquire_header_raw(&mut self, msg_len: usize) -> BytesMut {
        let mut h = self.headers.pop()
            .unwrap_or_else(|| BytesMut::with_capacity(self.config.header_initial_cap));
        h.clear();
        if msg_len > 0 {
            h.put_u32_le(msg_len as u32);
        }
        h
    }

    pub fn release_header(&mut self, mut buf: BytesMut) {
        if self.headers.len() < self.config.header_max_count {
            buf.clear();
            self.headers.push(buf);
        }
    }

    pub fn acquire_header(&mut self, len: usize) -> BytesMut {
        self.acquire_header_raw(len)
    }

    pub fn acquire_body(&mut self, needed_cap: usize) -> AlignedBuffer {

        let bucket_idx = self.config.buckets.iter().position(|b| b.size >= needed_cap);

        if let Some(idx) = bucket_idx {
            if let Some(buf) = self.body_buckets[idx].pop() {
                return buf;
            }
            return AlignedBuffer(AlignedVec::with_capacity(self.config.buckets[idx].size));
        }

        AlignedBuffer(AlignedVec::with_capacity(needed_cap))
    }

    pub fn release_body(&mut self, mut buf: AlignedBuffer) {
        let cap = buf.0.capacity();
        buf.0.clear();

        let alpha = self.config.ema_alpha;
        self.ema_size = (self.ema_size as f32 * (1.0 - alpha) + cap as f32 * alpha) as usize;

        if cap > (self.ema_size as f32 * self.config.ema_threshold_factor) as usize
            && cap > 131072 {
            return;
        }

        let bucket_idx = self.config.buckets.iter().rposition(|b| b.size <= cap);

        if let Some(idx) = bucket_idx {
            if self.body_buckets[idx].len() < self.config.buckets[idx].max_count {
                self.body_buckets[idx].push(buf);
            }
        }
    }

    pub fn release_mixed(&mut self, buf: Mixed) {
        match buf {
            Mixed::Bytes(h) => self.release_header(h),
            Mixed::AlignedBuffer(b) => self.release_body(b),
        }
    }
}


pub enum Mixed {
    Bytes(BytesMut),
    AlignedBuffer(AlignedBuffer),
}

impl IoBuf for Mixed {
    fn as_init(&self) -> &[u8] {
        match self {
            Mixed::Bytes(b) => b.as_init(),
            Mixed::AlignedBuffer(b) => b.as_init(),
        }
    }
}

impl SetLen for Mixed {
    unsafe fn set_len(&mut self, len: usize) {
        match self {
            Mixed::Bytes(b) => b.set_len(len),
            Mixed::AlignedBuffer(b) => b.set_len(len),
        }
    }
}

impl IoBufMut for Mixed {
    fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        match self {
            Mixed::Bytes(b) => b.as_uninit(),
            Mixed::AlignedBuffer(b) => b.as_uninit(),
        }
    }
}
