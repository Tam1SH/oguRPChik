use crate::codecs::base::OwnedBuf;
use compio::buf::{IoBuf, IoBufMut, SetLen};
use compio::io::{AsyncRead, AsyncWrite};
use socket2::SockAddr;
use std::fmt::Display;
use std::io;

pub mod adapters;
#[cfg(all(feature = "npipe", windows))]
pub mod npipe;
#[cfg(feature = "tcp")]
pub mod tcp;
#[cfg(feature = "uds")]
pub mod uds;
#[cfg(feature = "vsock")]
pub mod vsock;

pub trait StreamBuf: OwnedBuf + SetLen + IoBufMut + IoBuf {}
impl<T: OwnedBuf + SetLen + IoBufMut + IoBuf> StreamBuf for T {}

pub trait AsyncStream: Splitable + AsyncRead + AsyncWrite + Unpin + Clone + 'static {}

impl<T: Splitable + AsyncRead + AsyncWrite + Unpin + Clone + 'static> AsyncStream for T {}

pub trait Splitable {
    fn split(self) -> (Self, Self)
    where
        Self: Sized;
}

pub trait Connector: Send + Sync + 'static {
    type Stream: AsyncStream;
    async fn connect(&self) -> io::Result<Self::Stream>;
}

pub trait Acceptor: 'static {
    type Stream: AsyncStream;
    async fn accept(&self) -> io::Result<(Self::Stream, SockAddr)>;
}

pub trait AcceptorBuilder: Send + Sync + 'static {
    type Stream: AsyncStream;
    type Acceptor: Acceptor<Stream = Self::Stream>;
    async fn bind(self) -> io::Result<Self::Acceptor>;
    fn local_addr(&self) -> io::Result<impl Display>;
    fn kind(&self) -> &'static str;
}
