use rkyv::util::AlignedVec;
use serde::{Deserialize, Serialize};
use crate::codecs::serde_compatible::serde_format::{AlignedWriter, SerdeFormat};
use crate::codecs::serde_protocol::SerdeProtocol;

pub type JsonCodec<Req, Res> = SerdeProtocol<Req, Res, Json>;

pub struct Json;

impl SerdeFormat for Json {
    fn name() -> &'static str { "json" }

    fn serialize<T: Serialize, const A: usize>(value: &T, dest: &mut AlignedVec<A>) -> anyhow::Result<()> {
        let mut writer = AlignedWriter(dest);

        serde_json::to_writer(&mut writer, value).map_err(|e| anyhow::anyhow!(e))
    }

    fn deserialize<'a, T: Deserialize<'a>>(data: &'a [u8]) -> anyhow::Result<T> {
        serde_json::from_slice(data).map_err(|e| anyhow::anyhow!(e))
    }
}