use crate::transport::stream::{Acceptor, AcceptorBuilder, Connector, Splitable};
use compio::net::{UnixListener, UnixStream};
use socket2::SockAddr;
use std::fmt::Display;
use std::io;
use std::path::PathBuf;

impl Acceptor for UnixListener {
    type Stream = UnixStream;

    async fn accept(&self) -> io::Result<(Self::Stream, SockAddr)> {
        self.accept().await.map(|(s, a)| (s, a.into()))
    }
}

impl Splitable for UnixStream {
    fn split(self) -> (Self, Self) {
        (self.clone(), self)
    }
}

pub struct UdsAcceptorBuilder {
    pub path: PathBuf,
}

impl UdsAcceptorBuilder {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl AcceptorBuilder for UdsAcceptorBuilder {
    type Stream = UnixStream;
    type Acceptor = UnixListener;

    async fn bind(self) -> io::Result<Self::Acceptor> {
        let _ = std::fs::remove_file(&self.path);
        UnixListener::bind(&self.path).await
    }

    fn local_addr(&self) -> io::Result<impl Display> {
        Ok(self.path.display().to_string())
    }

    fn kind(&self) -> &'static str {
        "uds"
    }
}

pub struct UdsConnector {
    pub path: PathBuf,
}

impl UdsConnector {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Connector for UdsConnector {
    type Stream = UnixStream;

    async fn connect(&self) -> io::Result<Self::Stream> {
        UnixStream::connect(&self.path).await
    }
}