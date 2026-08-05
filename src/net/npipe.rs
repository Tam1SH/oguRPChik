//! Windows named pipes. Not gated behind a Cargo feature — gated by
//! `#[cfg(windows)]` at the point of use in `conn.rs`/`listener.rs`, since a
//! named pipe isn't a concept that exists on other platforms at all (unlike
//! uds, which compio supports on both Windows and Unix).

use crate::net::Splitable;
use compio::buf::{BufResult, IoBuf, IoBufMut};
use compio::fs::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions};
use compio::io::{AsyncRead, AsyncWrite};
use futures::lock::Mutex;
use std::fmt::{Display, Formatter};
use std::io;
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

/// A named-pipe listener. Unlike a socket listener, a Windows named pipe
/// server has to create the *next* pipe instance before handing off the one
/// that just connected, or a client racing to connect between two `accept()`
/// calls gets `ERROR_FILE_NOT_FOUND` instead of queuing.
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

    pub async fn bind(path: impl Into<String>) -> io::Result<Self> {
        let path = NamedPipePath::new(path);
        let server = Self::create_server(&path, true)?;
        Ok(Self {
            path,
            current: Arc::new(Mutex::new(server)),
        })
    }

    pub fn local_addr(&self) -> impl Display {
        self.path.clone()
    }

    pub async fn accept(&self) -> io::Result<NamedPipeStream> {
        let connected = {
            let mut guard = self.current.lock().await;
            let connected = guard.clone();
            let next = Self::create_server(&self.path, false)?;
            *guard = next;
            connected
        };

        connected.connect().await?;
        Ok(NamedPipeStream::Server(connected))
    }
}

pub async fn connect(path: impl Into<String>) -> io::Result<NamedPipeStream> {
    let path = NamedPipePath::new(path);
    ClientOptions::new()
        .open(&path.0)
        .await
        .map(NamedPipeStream::Client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use compio::io::{AsyncReadExt, AsyncWriteExt};

    fn pipe_name(name: &str) -> String {
        format!("ogurpchik-net-test-{}-{}", std::process::id(), name)
    }

    #[compio::test]
    async fn test_named_pipe_roundtrip() {
        let acceptor = NamedPipeAcceptor::bind(pipe_name("roundtrip"))
            .await
            .expect("bind failed");
        let endpoint = acceptor.local_addr().to_string();

        let accept_task = compio::runtime::spawn(async move {
            let mut stream = acceptor.accept().await.expect("accept failed");
            let BufResult(res, buf) = stream.read_exact(vec![0u8; 4]).await;
            res.expect("server read failed");
            assert_eq!(buf, b"ping");
            let BufResult(res, _) = stream.write_all(b"pong").await;
            res.expect("server write failed");
        });

        let mut client = connect(endpoint).await.expect("connect failed");
        let BufResult(res, _) = client.write_all(b"ping").await;
        res.expect("client write failed");
        let BufResult(res, buf) = client.read_exact(vec![0u8; 4]).await;
        res.expect("client read failed");
        assert_eq!(buf, b"pong");

        accept_task.await.unwrap();
    }

    #[compio::test]
    async fn test_named_pipe_reuses_listener() {
        let acceptor = NamedPipeAcceptor::bind(pipe_name("reuse"))
            .await
            .expect("bind failed");
        let endpoint = acceptor.local_addr().to_string();

        for expected in [b"one1", b"two2"] {
            let accept_future = acceptor.accept();
            let connect_future = connect(endpoint.clone());
            let (mut server_stream, mut client_stream) =
                futures::try_join!(accept_future, connect_future).expect("join failed");

            let BufResult(res, _) = client_stream.write_all(expected.as_slice()).await;
            res.expect("client write failed");
            let BufResult(res, buf) = server_stream.read_exact(vec![0u8; expected.len()]).await;
            res.expect("server read failed");
            assert_eq!(buf.as_slice(), expected.as_slice());
        }
    }
}
