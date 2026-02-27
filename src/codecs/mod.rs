use crate::align_buffer::AlignedBuffer;
use crate::message_codec::HandshakeCodec;
use crate::transport::discovery::Topology;

#[cfg(feature = "rkyv-codec")]
pub mod rkyv_protocol;
pub mod serde_protocol;
pub mod serde_compatible;

pub struct JsonHandshake;

impl HandshakeCodec for JsonHandshake {
    type Dest = AlignedBuffer;
    fn encode_handshake(topology: Option<&Topology>, dest: &mut AlignedBuffer) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(&topology)?;
        dest.0.extend_from_slice(&bytes);
        Ok(())
    }
    fn decode_handshake(data: &[u8]) -> anyhow::Result<Option<Topology>> {
        Ok(serde_json::from_slice(data)?)
    }
}
