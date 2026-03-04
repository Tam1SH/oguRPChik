use anyhow::Result;
use rkyv::util::AlignedVec;
use serde::{Deserialize, Serialize};
use std::io::{Result as IoResult, Write};

pub struct AlignedWriter<'a, const A: usize>(pub &'a mut AlignedVec<A>);

impl<'a, const A: usize> Write for AlignedWriter<'a, A> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    #[inline]
    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}

pub trait SerdeFormat: Send + Sync + 'static {
    fn name() -> &'static str;

    fn serialize<T: Serialize, const A: usize>(value: &T, dest: &mut AlignedVec<A>) -> Result<()>;

    fn deserialize<'a, T: Deserialize<'a>>(data: &'a [u8]) -> Result<T>;
}
