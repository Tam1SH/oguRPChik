use crate::auth::signed_process;
use crate::codecs::base::{BufferAllocator, OwnedBuf};
use crate::transport::base::{MessageSink, MessageSource};
use anyhow::{Context, bail};
use binrw::{BinRead, BinReaderExt, BinWrite, BinWriterExt};
use compio::time::timeout;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::Cursor;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const HANDSHAKE_VERSION: u16 = 1;

struct HandshakeConfig;

impl HandshakeConfig {
    const NONCE_LEN: usize = 32;
    const LABEL: &'static [u8] = b"ogurpchik/handshake/v1";
    const RETRY_ATTEMPTS: usize = 8;
    const STEP_TIMEOUT: Duration = Duration::from_millis(250);
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WireTag {
    Hello = 1,
    ClientAuth = 2,
    ServerAck = 3,
}

impl WireTag {
    fn byte(self) -> u8 {
        self as u8
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AckStatus {
    Ok = 0,
    Rejected = 1,
}

impl AckStatus {
    fn byte(self) -> u8 {
        self as u8
    }
}

type HmacSha256 = Hmac<Sha256>;

#[derive(BinRead, BinWrite)]
#[brw(little)]
struct HelloPacket {
    tag: u8,
    version: u16,
    scheme: u8,
    nonce_len: u16,
    #[br(count = nonce_len)]
    nonce: Vec<u8>,
}

#[derive(BinRead, BinWrite)]
#[brw(little)]
struct ClientAuthPacket {
    tag: u8,
    version: u16,
    scheme: u8,
    proof_len: u16,
    #[br(count = proof_len)]
    proof: Vec<u8>,
}

#[derive(BinRead, BinWrite)]
#[brw(little)]
struct AckPacket {
    tag: u8,
    status: u8,
    reason_len: u16,
    #[br(count = reason_len)]
    reason: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub enum HandshakeMode {
    #[default]
    Disabled,
    VersionOnly,
    HmacSha256 { secret: Arc<[u8]> },
    SignedProcess { public_key: Arc<[u8]> },
}

impl HandshakeMode {
    pub fn version_only() -> Self {
        Self::VersionOnly
    }

    pub fn hmac(secret: impl Into<Vec<u8>>) -> Self {
        Self::HmacSha256 {
            secret: Arc::<[u8]>::from(secret.into()),
        }
    }

    pub fn signed_process(public_key: impl Into<Vec<u8>>) -> Self {
        Self::SignedProcess {
            public_key: Arc::<[u8]>::from(public_key.into()),
        }
    }

    fn scheme_id(&self) -> u8 {
        match self {
            Self::Disabled | Self::VersionOnly => 0,
            Self::HmacSha256 { .. } => 1,
            Self::SignedProcess { .. } => 2,
        }
    }

    fn is_enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConnectionMode {
    OneToOne,
    #[default]
    OneToMany,
}

#[derive(Clone, Debug)]
pub struct ConnectionGate {
    mode: ConnectionMode,
    active: Arc<AtomicBool>,
}

impl ConnectionGate {
    pub fn new(mode: ConnectionMode) -> Self {
        Self {
            mode,
            active: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn mode(&self) -> ConnectionMode {
        self.mode
    }

    pub fn try_acquire(&self) -> Option<ConnectionLease> {
        match self.mode {
            ConnectionMode::OneToMany => Some(ConnectionLease {
                active: self.active.clone(),
                tracked: false,
            }),
            ConnectionMode::OneToOne => self
                .active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .ok()
                .map(|_| ConnectionLease {
                    active: self.active.clone(),
                    tracked: true,
                }),
        }
    }
}

pub struct ConnectionLease {
    active: Arc<AtomicBool>,
    tracked: bool,
}

impl fmt::Debug for ConnectionLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionLease")
            .field("tracked", &self.tracked)
            .finish()
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        if self.tracked {
            self.active.store(false, Ordering::Release);
        }
    }
}

pub async fn authenticate_client<Sink, Source, A>(
    sink: &Sink,
    source: &mut Source,
    allocator: &A,
    mode: &HandshakeMode,
) -> anyhow::Result<()>
where
    Sink: MessageSink<Payload = A::Payload>,
    Source: MessageSource,
    A: BufferAllocator,
{
    if !mode.is_enabled() {
        return Ok(());
    }

    let hello = recv_packet_with_timeout(
        source,
        HandshakeConfig::STEP_TIMEOUT * HandshakeConfig::RETRY_ATTEMPTS as u32,
    )
    .await
    .context("missing server handshake hello")?;
    if hello.first().copied() == Some(WireTag::ServerAck.byte()) {
        return decode_ack(&hello);
    }
    let (server_version, scheme, nonce) = decode_hello(&hello)?;

    if server_version != HANDSHAKE_VERSION {
        bail!(
            "Unsupported handshake version: server={} client={}",
            server_version,
            HANDSHAKE_VERSION
        );
    }

    if scheme != mode.scheme_id() {
        bail!(
            "Handshake auth mismatch: server={} client={}",
            scheme,
            mode.scheme_id()
        );
    }

    let proof = match mode {
        HandshakeMode::HmacSha256 { secret } => compute_hmac(secret, &nonce),
        HandshakeMode::SignedProcess { .. } => {
            signed_process::encode_auth_payload(std::process::id())
        }
        _ => Vec::new(),
    };

    let auth = encode_client_auth(HANDSHAKE_VERSION, scheme, &proof);
    send_packet(sink, allocator, &auth).await?;

    let ack = recv_packet_with_timeout(
        source,
        HandshakeConfig::STEP_TIMEOUT * HandshakeConfig::RETRY_ATTEMPTS as u32,
    )
    .await
    .context("missing server handshake ack")?;
    decode_ack(&ack)
}

pub async fn authenticate_server<Sink, Source, A>(
    sink: &Sink,
    source: &mut Source,
    allocator: &A,
    mode: &HandshakeMode,
) -> anyhow::Result<()>
where
    Sink: MessageSink<Payload = A::Payload>,
    Source: MessageSource,
    A: BufferAllocator,
{
    if !mode.is_enabled() {
        return Ok(());
    }

    let mut nonce = [0u8; HandshakeConfig::NONCE_LEN];
    getrandom::fill(&mut nonce)
        .map_err(|e| anyhow::anyhow!("failed to generate handshake nonce: {e}"))?;

    let hello = encode_hello(HANDSHAKE_VERSION, mode.scheme_id(), &nonce);
    let auth = {
        let mut response = None;
        for _ in 0..HandshakeConfig::RETRY_ATTEMPTS {
            send_packet(sink, allocator, &hello).await?;
            if let Some(packet) =
                recv_packet_with_timeout(source, HandshakeConfig::STEP_TIMEOUT).await
            {
                response = Some(packet);
                break;
            }
        }
        response.context("missing client handshake response")?
    };
    let (client_version, scheme, proof) = decode_client_auth(&auth)?;

    if client_version != HANDSHAKE_VERSION {
        let reason = format!(
            "Unsupported handshake version: client={} server={}",
            client_version, HANDSHAKE_VERSION
        );
        send_reject(sink, allocator, &reason).await?;
        bail!(reason);
    }

    if scheme != mode.scheme_id() {
        let reason = format!(
            "Handshake auth mismatch: client={} server={}",
            scheme,
            mode.scheme_id()
        );
        send_reject(sink, allocator, &reason).await?;
        bail!(reason);
    }

    match mode {
        HandshakeMode::HmacSha256 { secret } => {
            let expected = compute_hmac(secret, &nonce);
            if proof != expected {
                send_reject(sink, allocator, "Invalid handshake HMAC").await?;
                bail!("Invalid handshake HMAC");
            }
        }
        HandshakeMode::SignedProcess { public_key } => {
            let pid = signed_process::decode_auth_payload(&proof)?;
            if let Err(e) =
                signed_process::verify_process(pid, public_key).context("signed-process verification failed")
            {
                send_reject(sink, allocator, &e.to_string()).await?;
                return Err(e);
            }
        }
        _ if !proof.is_empty() => {
            send_reject(sink, allocator, "Unexpected auth proof").await?;
            bail!("Unexpected auth proof");
        }
        _ => {}
    }

    send_packet(sink, allocator, &encode_ack_ok()).await?;
    Ok(())
}

pub async fn reject_connection<Sink, A>(
    sink: &Sink,
    allocator: &A,
    reason: &str,
) -> anyhow::Result<()>
where
    Sink: MessageSink<Payload = A::Payload>,
    A: BufferAllocator,
{
    send_reject(sink, allocator, reason).await
}

async fn send_reject<Sink, A>(sink: &Sink, allocator: &A, reason: &str) -> anyhow::Result<()>
where
    Sink: MessageSink<Payload = A::Payload>,
    A: BufferAllocator,
{
    let packet = encode_ack_err(reason);
    send_packet(sink, allocator, &packet).await
}

async fn send_packet<Sink, A>(sink: &Sink, allocator: &A, bytes: &[u8]) -> anyhow::Result<()>
where
    Sink: MessageSink<Payload = A::Payload>,
    A: BufferAllocator,
{
    let mut payload = allocator.allocate(bytes.len());
    payload.write_from_slice(bytes);
    sink.send(payload).await
}

async fn recv_packet<Source>(source: &mut Source) -> Option<Vec<u8>>
where
    Source: MessageSource,
{
    source.recv().await.map(|payload| payload.as_ref().to_vec())
}

async fn recv_packet_with_timeout<Source>(
    source: &mut Source,
    duration: Duration,
) -> Option<Vec<u8>>
where
    Source: MessageSource,
{
    timeout(duration, recv_packet(source)).await.ok().flatten()
}

fn compute_hmac(secret: &[u8], nonce: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts arbitrary key lengths");
    mac.update(HandshakeConfig::LABEL);
    mac.update(&HANDSHAKE_VERSION.to_le_bytes());
    mac.update(nonce);
    mac.finalize().into_bytes().to_vec()
}

fn encode_hello(version: u16, scheme: u8, nonce: &[u8]) -> Vec<u8> {
    write_packet(&HelloPacket {
        tag: WireTag::Hello.byte(),
        version,
        scheme,
        nonce_len: nonce.len() as u16,
        nonce: nonce.to_vec(),
    })
}

fn decode_hello(bytes: &[u8]) -> anyhow::Result<(u16, u8, Vec<u8>)> {
    let packet: HelloPacket = read_packet(bytes)?;
    if packet.tag != WireTag::Hello.byte() {
        bail!("Invalid hello packet");
    }
    Ok((packet.version, packet.scheme, packet.nonce))
}

fn encode_client_auth(version: u16, scheme: u8, proof: &[u8]) -> Vec<u8> {
    write_packet(&ClientAuthPacket {
        tag: WireTag::ClientAuth.byte(),
        version,
        scheme,
        proof_len: proof.len() as u16,
        proof: proof.to_vec(),
    })
}

fn decode_client_auth(bytes: &[u8]) -> anyhow::Result<(u16, u8, Vec<u8>)> {
    let packet: ClientAuthPacket = read_packet(bytes)?;
    if packet.tag != WireTag::ClientAuth.byte() {
        bail!("Invalid client auth packet");
    }
    Ok((packet.version, packet.scheme, packet.proof))
}

fn encode_ack_ok() -> Vec<u8> {
    write_packet(&AckPacket {
        tag: WireTag::ServerAck.byte(),
        status: AckStatus::Ok.byte(),
        reason_len: 0,
        reason: Vec::new(),
    })
}

fn encode_ack_err(reason: &str) -> Vec<u8> {
    write_packet(&AckPacket {
        tag: WireTag::ServerAck.byte(),
        status: AckStatus::Rejected.byte(),
        reason_len: reason.len() as u16,
        reason: reason.as_bytes().to_vec(),
    })
}

fn decode_ack(bytes: &[u8]) -> anyhow::Result<()> {
    let packet: AckPacket = read_packet(bytes)?;
    if packet.tag != WireTag::ServerAck.byte() {
        bail!("Invalid handshake ack packet");
    }
    if packet.status == AckStatus::Ok.byte() {
        return Ok(());
    }
    let reason = String::from_utf8_lossy(&packet.reason).into_owned();
    bail!("Handshake rejected: {}", reason);
}

fn write_packet<T>(packet: &T) -> Vec<u8>
where
    T: BinWrite,
    for<'a> <T as BinWrite>::Args<'a>: Default,
{
    let mut cursor = Cursor::new(Vec::new());
    cursor
        .write_le(packet)
        .expect("handshake packet serialization should be infallible");
    cursor.into_inner()
}

fn read_packet<T>(bytes: &[u8]) -> anyhow::Result<T>
where
    T: BinRead,
    for<'a> <T as BinRead>::Args<'a>: Default,
{
    let mut cursor = Cursor::new(bytes);
    let packet = cursor.read_le().context("failed to decode handshake packet")?;
    if cursor.position() != bytes.len() as u64 {
        bail!("Malformed handshake packet");
    }
    Ok(packet)
}
