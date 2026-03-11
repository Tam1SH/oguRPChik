use anyhow::Result;
use bytes::BufMut;
use serde::{Deserialize, Serialize};

pub trait SerdeFormat: Send + Sync + 'static {
    fn name() -> &'static str;

    fn serialize<T: Serialize, B: BufMut>(value: &T, dest: &mut B) -> Result<()>;

    fn deserialize<'a, T: Deserialize<'a>>(data: &'a [u8]) -> Result<T>;
}
