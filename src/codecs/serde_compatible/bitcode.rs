use rkyv::util::AlignedVec;
use serde::{Deserialize, Serialize};
use crate::codecs::serde_compatible::serde_format::SerdeFormat;
use crate::codecs::serde_protocol::SerdeProtocol;

pub type BitcodeCodec<Req, Res> = SerdeProtocol<Req, Res, Bitcode>;

pub struct Bitcode;

impl SerdeFormat for Bitcode {
    fn name() -> &'static str { "bitcode" }

    fn serialize<T: Serialize, const A: usize>(value: &T, dest: &mut AlignedVec<A>) -> anyhow::Result<()> {

        let bytes = bitcode::serialize(value).map_err(|e| anyhow::anyhow!(e))?;
        dest.extend_from_slice(&bytes);
        Ok(())
    }

    fn deserialize<'a, T: Deserialize<'a>>(data: &'a [u8]) -> anyhow::Result<T> {
        bitcode::deserialize(data).map_err(|e| anyhow::anyhow!(e))
    }
}