use std::cell::Cell;
use std::marker::PhantomData;
use object_pool::Pool;
use crate::codecs::base::OwnedBuf;
use crate::pool::base::PoolStrategy;
use crate::transport::base::pool_config::PoolConfig;

thread_local! {
    static TL_STATE: std::cell::RefCell<anymap::AnyMap> =
        std::cell::RefCell::new(anymap::AnyMap::new());
}

pub struct TpcStrategy<B: OwnedBuf> {
    threshold_factor: f32,
    ema_alpha: f32,
    initial_cap: usize,
    _marker: PhantomData<B>,
}

impl<B: OwnedBuf> TpcStrategy<B> {
    pub fn new(initial_cap: usize, threshold_factor: f32, ema_alpha: f32) -> Self {
        Self {
            initial_cap,
            threshold_factor,
            ema_alpha,
            _marker: PhantomData,
        }
    }

    fn with_tl<R>(&self, f: impl FnOnce(&Pool<B>, &Cell<f32>) -> R) -> R {
        TL_STATE.with(|state| {
            let mut map = state.borrow_mut();

            if map.get::<Pool<B>>().is_none() {
                map.insert(Pool::<B>::new(0, || B::with_capacity(self.initial_cap)));
                map.insert(Cell::new(self.initial_cap as f32));
            }

            let pool = map.get::<Pool<B>>().unwrap();
            let ema = map.get::<Cell<f32>>().unwrap();
            f(pool, ema)
        })
    }
}

impl<B: OwnedBuf> Clone for TpcStrategy<B> {
    fn clone(&self) -> Self {
        Self {
            threshold_factor: self.threshold_factor,
            ema_alpha: self.ema_alpha,
            initial_cap: self.initial_cap,
            _marker: PhantomData,
        }
    }
}

impl<B: OwnedBuf> PoolStrategy<B> for TpcStrategy<B> {
    type SendMark = *const ();

    fn get(config: &PoolConfig) -> Self {
        Self {
            initial_cap: config.initial_cap,
            threshold_factor: config.threshold_factor,
            ema_alpha: config.ema_alpha,
            _marker: PhantomData,
        }
    }

    fn acquire(&self, cap: usize) -> B {
        self.with_tl(|pool, _ema| {
            let mut buf = pool.pull(|| B::with_capacity(cap)).detach().1;
            if buf.capacity() < cap {
                buf = B::with_capacity(cap);
            }
            buf.clear();
            buf
        })
    }

    fn release(&self, mut buf: B) {
        let cap = buf.capacity();
        let alpha = self.ema_alpha;
        let threshold = self.threshold_factor;

        self.with_tl(|pool, ema_cell| {
            let new_ema = ema_cell.get() * (1.0 - alpha) + cap as f32 * alpha;
            ema_cell.set(new_ema);

            if cap > (new_ema * threshold) as usize && cap > 131072 {
                return;
            }

            buf.clear();
            pool.attach(buf);
        });
    }
}