use std::fmt::Display;
use crate::transport::stream::{Acceptor, AcceptorBuilder, Connector, Splitable};
use compio::net::{TcpListener, TcpStream};
use socket2::SockAddr;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};

impl Acceptor for TcpListener {
    type Stream = TcpStream;

    async fn accept(&self) -> io::Result<(Self::Stream, SockAddr)> {
        self.accept().await.map(|(s, a)| (s, a.into()))
    }
}

impl Splitable for TcpStream {
    fn split(self) -> (Self, Self) {
        (self.clone(), self)
    }
}

pub struct TcpAcceptorBuilder {
    pub addr: SocketAddr,
}

impl TcpAcceptorBuilder {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }
}

impl AcceptorBuilder for TcpAcceptorBuilder {
    type Stream = TcpStream;
    type Acceptor = TcpListener;

    fn local_addr(&self) -> io::Result<impl Display> {
        Ok(self.addr.to_string())
    }

    async fn bind(self) -> io::Result<Self::Acceptor> {
        TcpListener::bind(self.addr).await
    }

    fn kind(&self) -> &'static str {
        "tcp"
    }
}

pub struct TcpConnector {
    pub addr: SocketAddr,
}

impl TcpConnector {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }
}

impl Connector for TcpConnector {
    type Stream = TcpStream;

    async fn connect(&self) -> io::Result<Self::Stream> {
        TcpStream::connect(self.addr).await
    }
}
