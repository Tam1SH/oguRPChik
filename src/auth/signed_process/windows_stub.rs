use anyhow::bail;

pub(crate) fn verify_process(_: u32, _: &[u8]) -> anyhow::Result<()> {
    bail!("SignedProcess handshake is only supported on Windows")
}
