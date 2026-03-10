use bytes::BytesMut;
use compio::buf::{IoBuf, IoBufMut, SetLen};
use std::mem::MaybeUninit;
use crate::codecs::base::OwnedBuf;
use crate::codecs::rkyv_protocol::AlignedBuffer;
use crate::codecs::serde_protocol::VecBuf;
use crate::transport::stream::StreamBuf;

pub struct Frame<B: StreamBuf> {
    pub header: BytesMut,
    pub body: B,
}

pub(crate) enum Slot<B: StreamBuf> {
    Header(BytesMut),
    Body(B),
}

pub struct FrameBatch<B: StreamBuf> {
    slots: Vec<Slot<B>>,
}


impl<B: StreamBuf> IoBuf for Slot<B> {
    fn as_init(&self) -> &[u8] {
        match self {
            Slot::Header(h) => h.as_init(),
            Slot::Body(b) => b.as_ref(),
        }
    }
}

impl<B: StreamBuf> SetLen for Slot<B> {
    unsafe fn set_len(&mut self, len: usize) {
        unsafe {
            match self {
                Slot::Header(h) => h.set_len(len),
                Slot::Body(_) => {}
            }
        }
    }
}

impl<B: StreamBuf> IoBufMut for Slot<B> {
    fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        match self {
            Slot::Header(h) => h.as_uninit(),
            Slot::Body(_) => &mut [],
        }
    }
}


impl<B: StreamBuf> FrameBatch<B> {
    pub fn with_capacity(frames: usize) -> Self {
        Self {
            slots: Vec::with_capacity(frames * 2),
        }
    }

    pub fn push(&mut self, frame: Frame<B>) {
        self.slots.push(Slot::Header(frame.header));
        self.slots.push(Slot::Body(frame.body));
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn frame_count(&self) -> usize {
        self.slots.len() / 2
    }

    pub(crate) fn into_slots(self) -> Vec<Slot<B>> {
        self.slots
    }

    pub(crate) fn from_slots(slots: Vec<Slot<B>>) -> Self {
        Self { slots }
    }

    pub fn drain_frames(&mut self) -> DrainFrames<'_, B> {
        DrainFrames {
            inner: self.slots.drain(..),
        }
    }
}

pub struct DrainFrames<'a, B: StreamBuf> {
    inner: std::vec::Drain<'a, Slot<B>>,
}

impl<'a, B: StreamBuf> Iterator for DrainFrames<'a, B> {
    type Item = Frame<B>;

    fn next(&mut self) -> Option<Frame<B>> {
        let header = match self.inner.next()? {
            Slot::Header(h) => h,
            Slot::Body(_) => panic!("FrameBatch slot ordering violated: expected Header"),
        };
        let body = match self.inner.next()? {
            Slot::Body(b) => b,
            Slot::Header(_) => panic!("FrameBatch slot ordering violated: expected Body"),
        };
        Some(Frame { header, body })
    }
}


impl IoBuf for AlignedBuffer {
    fn as_init(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl SetLen for AlignedBuffer {
    unsafe fn set_len(&mut self, len: usize) {
        unsafe { self.0.set_len(len) };
    }
}

impl IoBufMut for AlignedBuffer {
    fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        unsafe {
            let len = self.0.len();
            let capacity = self.0.capacity();

            let ptr = self.0.as_mut_ptr().add(len) as *mut MaybeUninit<u8>;

            std::slice::from_raw_parts_mut(ptr, capacity - len)
        }
    }
}

impl IoBuf for VecBuf {
    fn as_init(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for VecBuf {
    fn from(buf: Vec<u8>) -> Self {
        Self(buf)
    }
}
impl SetLen for VecBuf {
    unsafe fn set_len(&mut self, len: usize) {
        unsafe { self.0.set_len(len) }
    }
}

impl IoBufMut for VecBuf {
    fn as_uninit(&mut self) -> &mut [MaybeUninit<u8>] {
        let len = self.0.len();
        let cap = self.0.capacity();
        let ptr = self.0.as_mut_ptr() as *mut MaybeUninit<u8>;
        unsafe { std::slice::from_raw_parts_mut(ptr.add(len), cap - len) }
    }
}
