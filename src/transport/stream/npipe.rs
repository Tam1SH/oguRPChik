use crate::transport::stream::{Acceptor, AcceptorBuilder, Connector, Splitable};
use compio::buf::{BufResult, IoBuf, IoBufMut};
use compio::fs::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions};
use compio::io::{AsyncRead, AsyncWrite};
use futures::lock::Mutex;
use socket2::SockAddr;
use std::fmt::{Display, Formatter};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum NamedPipeStream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl AsyncRead for NamedPipeStream {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        match self {
            Self::Server(stream) => stream.read(buf).await,
            Self::Client(stream) => stream.read(buf).await,
        }
    }
}

impl AsyncWrite for NamedPipeStream {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        match self {
            Self::Server(stream) => stream.write(buf).await,
            Self::Client(stream) => stream.write(buf).await,
        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Server(stream) => stream.flush().await,
            Self::Client(stream) => stream.flush().await,
        }
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        match self {
            Self::Server(stream) => stream.shutdown().await,
            Self::Client(stream) => stream.shutdown().await,
        }
    }
}

impl Splitable for NamedPipeStream {
    fn split(self) -> (Self, Self) {
        (self.clone(), self)
    }
}

#[derive(Debug, Clone)]
pub struct NamedPipePath(String);

impl NamedPipePath {
    pub fn new(path: impl Into<String>) -> Self {
        let path = path.into();
        if path.starts_with(r"\\.\pipe\") {
            Self(path)
        } else {
            Self(format!(r"\\.\pipe\{}", path.trim_start_matches('\\')))
        }
    }
}

impl Display for NamedPipePath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub struct NamedPipeAcceptor {
    path: NamedPipePath,
    current: Arc<Mutex<NamedPipeServer>>,
}

impl NamedPipeAcceptor {
    fn create_server(path: &NamedPipePath, first_instance: bool) -> io::Result<NamedPipeServer> {
        let mut options = ServerOptions::new();
        options.first_pipe_instance(first_instance);
        options.create(&path.0)
    }
}

impl Acceptor for NamedPipeAcceptor {
    type Stream = NamedPipeStream;

    async fn accept(&self) -> io::Result<(Self::Stream, SockAddr)> {
        let connected = {
            let mut guard = self.current.lock().await;
            let connected = guard.clone();
            let next = Self::create_server(&self.path, false)?;
            *guard = next;
            connected
        };

        connected.connect().await?;
        Ok((NamedPipeStream::Server(connected), loopback_addr()))
    }
}

pub struct NamedPipeAcceptorBuilder {
    pub path: NamedPipePath,
}

impl NamedPipeAcceptorBuilder {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: NamedPipePath::new(path),
        }
    }
}

impl AcceptorBuilder for NamedPipeAcceptorBuilder {
    type Stream = NamedPipeStream;
    type Acceptor = NamedPipeAcceptor;

    async fn bind(self) -> io::Result<Self::Acceptor> {
        let server = NamedPipeAcceptor::create_server(&self.path, true)?;
        Ok(NamedPipeAcceptor {
            path: self.path,
            current: Arc::new(Mutex::new(server)),
        })
    }

    fn local_addr(&self) -> io::Result<impl Display> {
        Ok(self.path.clone())
    }

    fn kind(&self) -> &'static str {
        "npipe"
    }
}

pub struct NamedPipeConnector {
    pub path: NamedPipePath,
}

impl NamedPipeConnector {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: NamedPipePath::new(path),
        }
    }
}

impl Connector for NamedPipeConnector {
    type Stream = NamedPipeStream;

    async fn connect(&self) -> io::Result<Self::Stream> {
        ClientOptions::new()
            .open(&self.path.0)
            .await
            .map(NamedPipeStream::Client)
    }
}

fn loopback_addr() -> SockAddr {
    SockAddr::from(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use compio::io::{AsyncReadExt, AsyncWriteExt};

    fn pipe_name(name: &str) -> String {
        format!("ogurpchik-test-{}-{}", std::process::id(), name)
    }

    #[compio::test]
    async fn test_named_pipe_acceptor_roundtrip() {
        let builder = NamedPipeAcceptorBuilder::new(pipe_name("roundtrip"));
        let endpoint = builder.local_addr().unwrap().to_string();
        let acceptor = builder.bind().await.expect("bind failed");

        let accept_task = compio::runtime::spawn(async move {
            let (mut stream, _) = acceptor.accept().await.expect("accept failed");
            let BufResult(res, buf) = stream.read_exact(vec![0u8; 4]).await;
            res.expect("server read failed");
            assert_eq!(buf, b"ping");
            let BufResult(res, _) = stream.write_all(b"pong").await;
            res.expect("server write failed");
        });

        let mut client = NamedPipeConnector::new(endpoint)
            .connect()
            .await
            .expect("connect failed");
        let BufResult(res, _) = client.write_all(b"ping").await;
        res.expect("client write failed");
        let BufResult(res, buf) = client.read_exact(vec![0u8; 4]).await;
        res.expect("client read failed");
        assert_eq!(buf, b"pong");

        accept_task.await.unwrap();
    }

    #[compio::test]
    async fn test_named_pipe_acceptor_reuses_listener() {
        let builder = NamedPipeAcceptorBuilder::new(pipe_name("reuse"));
        let endpoint = builder.local_addr().unwrap().to_string();
        let acceptor = builder.bind().await.expect("bind failed");

        for expected in [b"one1", b"two2"] {
            let accept_future = acceptor.accept();
            let connector = NamedPipeConnector::new(endpoint.clone());
            let connect_future = connector.connect();
            let ((mut server_stream, _), mut client_stream) =
                futures::try_join!(accept_future, connect_future).expect("join failed");

            let BufResult(res, _) = client_stream.write_all(expected.as_slice()).await;
            res.expect("client write failed");
            let BufResult(res, buf) = server_stream.read_exact(vec![0u8; expected.len()]).await;
            res.expect("server read failed");
            assert_eq!(buf.as_slice(), expected.as_slice());
        }
    }
}
