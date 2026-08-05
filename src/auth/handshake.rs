
use crate::auth::signed_process;
use crate::error::{HandshakeError, Result, TransportError};
use crate::net::{Conn, PeerIdentity};
use binrw::{BinRead, BinReaderExt, BinWrite, BinWriterExt};
use compio::BufResult;
use compio::io::{AsyncReadExt, AsyncWriteExt};
use compio::time::timeout;
use error_stack::{Report, ResultExt};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::cell::Cell;
use std::fmt;
use std::io::Cursor;
use std::rc::Rc;
use std::time::Duration;

const HANDSHAKE_VERSION: u16 = 1;
const NONCE_LEN: usize = 32;
const HMAC_LABEL: &[u8] = b"ogurpchik/handshake/v1";

const STEP_TIMEOUT: Duration = Duration::from_secs(5);

const MAX_PACKET_LEN: u32 = 4096;

const TAG_HELLO: u8 = 1;
const TAG_CLIENT_AUTH: u8 = 2;
const TAG_ACK: u8 = 3;

const ACK_OK: u8 = 0;
const ACK_REJECTED: u8 = 1;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, Default)]
pub enum HandshakeMode {
    #[default]
    Disabled,
    VersionOnly,
    HmacSha256 { secret: Rc<[u8]> },
    SignedProcess { public_key: Rc<[u8]> },
}

impl HandshakeMode {
    pub fn version_only() -> Self {
        Self::VersionOnly
    }

    pub fn hmac(secret: impl Into<Vec<u8>>) -> Self {
        Self::HmacSha256 {
            secret: Rc::<[u8]>::from(secret.into()),
        }
    }

    pub fn signed_process(public_key: impl Into<Vec<u8>>) -> Self {
        Self::SignedProcess {
            public_key: Rc::<[u8]>::from(public_key.into()),
        }
    }

    fn scheme_id(&self) -> u8 {
        match self {
            Self::Disabled | Self::VersionOnly => 0,
            Self::HmacSha256 { .. } => 1,
            Self::SignedProcess { .. } => 2,
        }
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
    active: Rc<Cell<bool>>,
}

impl ConnectionGate {
    pub fn new(mode: ConnectionMode) -> Self {
        Self {
            mode,
            active: Rc::new(Cell::new(false)),
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
            ConnectionMode::OneToOne if !self.active.get() => {
                self.active.set(true);
                Some(ConnectionLease {
                    active: self.active.clone(),
                    tracked: true,
                })
            }
            ConnectionMode::OneToOne => None,
        }
    }
}

pub struct ConnectionLease {
    active: Rc<Cell<bool>>,
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
            self.active.set(false);
        }
    }
}

pub async fn authenticate_server(conn: &mut Conn, mode: &HandshakeMode) -> Result<(), HandshakeError> {
    if matches!(mode, HandshakeMode::Disabled) {
        return Ok(());
    }

    let attested_pid = match mode {
        HandshakeMode::SignedProcess { .. } => match conn.peer_identity() {
            PeerIdentity::Pid { pid } => Some(pid),
            PeerIdentity::Unknown => {
                return Err(Report::new(HandshakeError::PeerAttestationUnavailable)
                    .attach(format!("transport: {}", conn.kind())));
            }
        },
        _ => None,
    };

    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| {
        Report::new(HandshakeError::Io).attach(format!("failed to generate nonce: {e}"))
    })?;

    write_packet(conn, &encode_hello(mode.scheme_id(), &nonce)).await?;
    let auth_body = read_packet_with_timeout(conn).await?;
    let (client_version, scheme, proof) = decode_client_auth(&auth_body)?;

    if client_version != HANDSHAKE_VERSION {
        let reason = format!(
            "unsupported handshake version: client={client_version} server={HANDSHAKE_VERSION}"
        );
        let _ = write_packet(conn, &encode_ack(ACK_REJECTED, reason.as_bytes())).await;
        return Err(Report::new(HandshakeError::UnsupportedVersion).attach(reason));
    }

    if scheme != mode.scheme_id() {
        let reason = format!(
            "handshake auth mismatch: client={scheme} server={}",
            mode.scheme_id()
        );
        let _ = write_packet(conn, &encode_ack(ACK_REJECTED, reason.as_bytes())).await;
        return Err(Report::new(HandshakeError::SchemeMismatch).attach(reason));
    }

    match mode {
        HandshakeMode::HmacSha256 { secret } => {
            let mac = hmac_with_nonce(secret, &nonce);
            if mac.verify_slice(&proof).is_err() {
                let _ = write_packet(conn, &encode_ack(ACK_REJECTED, b"invalid handshake proof")).await;
                return Err(Report::new(HandshakeError::InvalidProof));
            }
        }
        HandshakeMode::SignedProcess { public_key } => {
            let pid = attested_pid.expect("attestation checked before hello");
            if let Err(report) = verify_signed_process(pid, public_key) {
                let _ = write_packet(
                    conn,
                    &encode_ack(ACK_REJECTED, b"signed-process verification failed"),
                )
                .await;
                return Err(report);
            }
        }
        _ if !proof.is_empty() => {
            let _ = write_packet(conn, &encode_ack(ACK_REJECTED, b"unexpected auth proof")).await;
            return Err(Report::new(HandshakeError::InvalidProof)
                .attach("mode carries no proof, but the client sent one"));
        }
        _ => {}
    }

    write_packet(conn, &encode_ack(ACK_OK, &[])).await
}

pub async fn authenticate_client(conn: &mut Conn, mode: &HandshakeMode) -> Result<(), HandshakeError> {
    if matches!(mode, HandshakeMode::Disabled) {
        return Ok(());
    }

    let hello_body = read_packet_with_timeout(conn).await?;
    if hello_body.first() == Some(&TAG_ACK) {
        return decode_ack(&hello_body);
    }
    let (server_version, scheme, nonce) = decode_hello(&hello_body)?;

    if server_version != HANDSHAKE_VERSION {
        return Err(Report::new(HandshakeError::UnsupportedVersion)
            .attach(format!("server={server_version} client={HANDSHAKE_VERSION}")));
    }

    if scheme != mode.scheme_id() {
        return Err(Report::new(HandshakeError::SchemeMismatch)
            .attach(format!("server={scheme} client={}", mode.scheme_id())));
    }

    let proof = match mode {
        HandshakeMode::HmacSha256 { secret } => hmac_with_nonce(secret, &nonce)
            .finalize()
            .into_bytes()
            .to_vec(),
        _ => Vec::new(),
    };
    write_packet(conn, &encode_client_auth(scheme, &proof)).await?;

    let ack_body = read_packet_with_timeout(conn).await?;
    decode_ack(&ack_body)
}

pub async fn reject_connection(conn: &mut Conn, reason: &str) -> Result<(), HandshakeError> {
    write_packet(conn, &encode_ack(ACK_REJECTED, reason.as_bytes())).await
}

fn verify_signed_process(pid: u32, public_key: &[u8]) -> Result<(), HandshakeError> {
    let created_before = signed_process::process_creation_time(pid)?;
    signed_process::verify_process_image(pid, public_key)?;
    let created_after = signed_process::process_creation_time(pid)?;
    if created_after != created_before {
        return Err(Report::new(HandshakeError::SignedProcessVerificationFailed)
            .attach(format!("pid {pid} was reused during verification")));
    }
    Ok(())
}

fn hmac_with_nonce(secret: &[u8], nonce: &[u8]) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts arbitrary key lengths");
    mac.update(HMAC_LABEL);
    mac.update(HANDSHAKE_VERSION.to_le_bytes().as_slice());
    mac.update(nonce);
    mac
}

async fn write_packet(conn: &mut Conn, body: &[u8]) -> Result<(), HandshakeError> {
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(body);
    let BufResult(res, _) = conn.write_all(frame).await;
    res.change_context(TransportError::Io)
        .change_context(HandshakeError::Io)
}

async fn read_packet(conn: &mut Conn) -> Result<Vec<u8>, HandshakeError> {
    let BufResult(res, header) = conn.read_exact([0u8; 4]).await;
    res.change_context(TransportError::Io)
        .change_context(HandshakeError::Io)?;
    let len = u32::from_le_bytes(header);
    if len > MAX_PACKET_LEN {
        return Err(Report::new(HandshakeError::MalformedPacket)
            .attach(format!("declared packet length {len} exceeds {MAX_PACKET_LEN}")));
    }
    let BufResult(res, body) = conn.read_exact(vec![0u8; len as usize]).await;
    res.change_context(TransportError::Io)
        .change_context(HandshakeError::Io)?;
    Ok(body)
}

async fn read_packet_with_timeout(conn: &mut Conn) -> Result<Vec<u8>, HandshakeError> {
    match timeout(STEP_TIMEOUT, read_packet(conn)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(Report::new(HandshakeError::Timeout)),
    }
}

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

fn encode_hello(scheme: u8, nonce: &[u8]) -> Vec<u8> {
    write_body(&HelloPacket {
        tag: TAG_HELLO,
        version: HANDSHAKE_VERSION,
        scheme,
        nonce_len: nonce.len() as u16,
        nonce: nonce.to_vec(),
    })
}

fn decode_hello(body: &[u8]) -> Result<(u16, u8, Vec<u8>), HandshakeError> {
    let packet: HelloPacket = read_body(body, "hello")?;
    if packet.tag != TAG_HELLO {
        return Err(malformed("hello"));
    }
    Ok((packet.version, packet.scheme, packet.nonce))
}

fn encode_client_auth(scheme: u8, proof: &[u8]) -> Vec<u8> {
    write_body(&ClientAuthPacket {
        tag: TAG_CLIENT_AUTH,
        version: HANDSHAKE_VERSION,
        scheme,
        proof_len: proof.len() as u16,
        proof: proof.to_vec(),
    })
}

fn decode_client_auth(body: &[u8]) -> Result<(u16, u8, Vec<u8>), HandshakeError> {
    let packet: ClientAuthPacket = read_body(body, "client auth")?;
    if packet.tag != TAG_CLIENT_AUTH {
        return Err(malformed("client auth"));
    }
    Ok((packet.version, packet.scheme, packet.proof))
}

fn encode_ack(status: u8, reason: &[u8]) -> Vec<u8> {
    write_body(&AckPacket {
        tag: TAG_ACK,
        status,
        reason_len: reason.len() as u16,
        reason: reason.to_vec(),
    })
}

fn decode_ack(body: &[u8]) -> Result<(), HandshakeError> {
    let packet: AckPacket = read_body(body, "ack")?;
    if packet.tag != TAG_ACK {
        return Err(malformed("ack"));
    }
    if packet.status == ACK_OK {
        return Ok(());
    }
    let reason = String::from_utf8_lossy(&packet.reason).into_owned();
    Err(Report::new(HandshakeError::Rejected).attach(reason))
}

fn write_body<T>(packet: &T) -> Vec<u8>
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

fn read_body<T>(body: &[u8], what: &str) -> Result<T, HandshakeError>
where
    T: BinRead,
    for<'a> <T as BinRead>::Args<'a>: Default,
{
    let mut cursor = Cursor::new(body);
    let packet = cursor.read_le().map_err(|_| malformed(what))?;
    if cursor.position() != body.len() as u64 {
        return Err(malformed(what));
    }
    Ok(packet)
}

fn malformed(what: &str) -> Report<HandshakeError> {
    Report::new(HandshakeError::MalformedPacket).attach(format!("invalid {what} packet"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Listener;

    async fn tcp_pair() -> (Conn, Conn) {
        let listener = Listener::bind_tcp("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind failed");
        let Listener::Tcp(inner) = &listener else {
            unreachable!()
        };
        let addr = inner.local_addr().expect("local_addr failed");
        let (server, client) = futures::try_join!(listener.accept(), Conn::connect_tcp(addr))
            .expect("join failed");
        drop(listener);
        (server, client)
    }

    #[cfg(windows)]
    async fn npipe_pair(name: &str) -> (Conn, Conn) {
        let listener = Listener::bind_npipe(name).await.expect("bind failed");
        let (server, client) =
            futures::try_join!(listener.accept(), Conn::connect_npipe(name)).expect("join failed");
        drop(listener);
        (server, client)
    }

    #[compio::test]
    async fn hmac_handshake_ok() {
        let (mut server, mut client) = tcp_pair().await;
        let server_fut = compio::runtime::spawn(async move {
            authenticate_server(&mut server, &HandshakeMode::hmac(b"shared-secret".to_vec())).await
        });
        let client_result =
            authenticate_client(&mut client, &HandshakeMode::hmac(b"shared-secret".to_vec())).await;
        client_result.expect("client handshake failed");
        server_fut.await.unwrap().expect("server handshake failed");
    }

    #[compio::test]
    async fn hmac_wrong_secret_is_rejected() {
        let (mut server, mut client) = tcp_pair().await;
        let server_fut = compio::runtime::spawn(async move {
            authenticate_server(&mut server, &HandshakeMode::hmac(b"right".to_vec())).await
        });
        let client_result =
            authenticate_client(&mut client, &HandshakeMode::hmac(b"wrong".to_vec())).await;
        let client_err = client_result.expect_err("client must be rejected");
        assert!(matches!(
            client_err.current_context(),
            HandshakeError::Rejected
        ));
        let server_err = server_fut.await.unwrap().expect_err("server must reject");
        assert!(matches!(
            server_err.current_context(),
            HandshakeError::InvalidProof
        ));
    }

    #[compio::test]
    async fn scheme_mismatch_is_detected_by_client() {
        let (mut server, mut client) = tcp_pair().await;
        let server_fut = compio::runtime::spawn(async move {
            authenticate_server(&mut server, &HandshakeMode::hmac(b"s".to_vec())).await
        });
        let client_result =
            authenticate_client(&mut client, &HandshakeMode::version_only()).await;
        let client_err = client_result.expect_err("client must detect the scheme mismatch");
        assert!(matches!(
            client_err.current_context(),
            HandshakeError::SchemeMismatch
        ));
        drop(server_fut);
    }

    #[compio::test]
    async fn signed_process_refused_on_unattestable_transport() {
        let (mut server, _client) = tcp_pair().await;
        let err = authenticate_server(&mut server, &HandshakeMode::signed_process(vec![0u8; 32]))
            .await
            .expect_err("signed-process on tcp must be refused");
        assert!(matches!(
            err.current_context(),
            HandshakeError::PeerAttestationUnavailable
        ));
    }

    #[cfg(windows)]
    #[compio::test]
    async fn signed_process_uses_os_pid_not_wire_pid() {
        use base64::Engine;
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let public_key = signing_key.verifying_key().to_bytes().to_vec();
        let image_path = std::env::current_exe().expect("current exe");
        let image_bytes = std::fs::read(&image_path).expect("read own image");
        let signature = signing_key.sign(&image_bytes);
        let sig_path = {
            let mut os = image_path.as_os_str().to_owned();
            os.push(".sig");
            std::path::PathBuf::from(os)
        };
        std::fs::write(
            &sig_path,
            base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
        )
        .expect("write detached signature");

        let server_mode = HandshakeMode::signed_process(public_key.clone());
        let client_mode = HandshakeMode::signed_process(public_key.clone());

        {
            let name = format!("ogurpchik-hs-signed-{}", std::process::id());
            let (mut server, mut client) = npipe_pair(&name).await;
            let server_fut = compio::runtime::spawn(async move {
                authenticate_server(&mut server, &server_mode).await
            });
            authenticate_client(&mut client, &client_mode)
                .await
                .expect("client handshake failed");
            server_fut.await.unwrap().expect("server handshake failed");
        }

        {
            let name = format!("ogurpchik-hs-forged-{}", std::process::id());
            let (mut server, mut client) = npipe_pair(&name).await;
            let server_fut = compio::runtime::spawn(async move {
                authenticate_server(&mut server, &HandshakeMode::signed_process(public_key.clone()))
                    .await
            });

            let hello_body = read_packet_with_timeout(&mut client)
                .await
                .expect("read hello");
            let (_version, scheme, _nonce) = decode_hello(&hello_body).expect("decode hello");
            let forged_proof = 0xDEADu32.to_le_bytes();
            write_packet(&mut client, &encode_client_auth(scheme, &forged_proof))
                .await
                .expect("send forged auth");
            let ack_body = read_packet_with_timeout(&mut client)
                .await
                .expect("read ack");
            decode_ack(&ack_body).expect("forged wire PID must be ignored, not trusted");
            server_fut.await.unwrap().expect("server handshake failed");
        }

        let _ = std::fs::remove_file(&sig_path);
    }
}
