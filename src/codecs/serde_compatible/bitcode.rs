use crate::codecs::serde_compatible::serde_format::SerdeFormat;
use crate::codecs::serde_protocol::SerdeProtocol;
use bytes::BufMut;
use serde::{Deserialize, Serialize};
use std::io::Write;

pub type BitcodeCodec<Req, Res> = SerdeProtocol<Req, Res, Bitcode>;

pub struct Bitcode;

impl SerdeFormat for Bitcode {
    fn name() -> &'static str {
        "bitcode"
    }

    fn serialize<T: Serialize, B: BufMut>(value: &T, dest: &mut B) -> anyhow::Result<()> {
        let bytes = bitcode::serialize(value).map_err(|e| anyhow::anyhow!(e))?;
        let mut writer = dest.writer();
        writer.write(&bytes)?;
        Ok(())
    }

    fn deserialize<'a, T: Deserialize<'a>>(data: &'a [u8]) -> anyhow::Result<T> {
        bitcode::deserialize(data).map_err(|e| anyhow::anyhow!(e))
    }
}
