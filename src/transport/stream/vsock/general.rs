use crate::transport::stream::Splitable;
use compio::buf::{IoBuf, IoBufMut};
use compio::io::{AsyncRead, AsyncWrite};
use compio::BufResult;
use socket2::SockAddr;
use std::io;
use tracing::{debug, error, info, instrument, trace};
use crate::transport::stream::vsock::VsockTarget;

#[derive(Clone)]
pub enum VStream {
    #[cfg(windows)]
    Hv(crate::transport::stream::vsock::windows::HvStream),

    #[cfg(unix)]
    Vsock(crate::transport::stream::vsock::linux::VsockStream),
}

impl Splitable for VStream {
    fn split(self) -> (Self, Self) {
        let clone = match &self {
            #[cfg(windows)]
            Self::Hv(s) => Self::Hv(s.clone()),
            #[cfg(unix)]
            Self::Vsock(s) => Self::Vsock(s.clone()),
        };
        (clone, self)
    }
}

impl VStream {

    pub async fn connect_loopback(port: u32) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Self::connect(VsockTarget::Cid(libc::VMADDR_CID_LOCAL), port).await
        }
        #[cfg(windows)]
        {
            Self::connect(VsockTarget::Cid(1), port).await
        }
    }
    pub async fn connect(target: VsockTarget, port: u32) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let cid = match target {
                VsockTarget::Cid(c) => c,
                VsockTarget::Guid(_) => return Err(io::Error::new(io::ErrorKind::InvalidInput, "UUID not supported on Unix")),
            };
            crate::transport::stream::vsock::linux::VsockStream::connect(cid, port).await
                .map(Self::Vsock)
        }

        #[cfg(windows)]
        {
            use crate::transport::stream::vsock::windows::ToServiceId;
            use crate::transport::stream::vsock::utils::uuid_to_guid;
            let (vm_guid, service_id) = match target {
                VsockTarget::Cid(cid) => {
                    let g = match cid {
                        u32::MAX => ::windows::Win32::System::Hypervisor::HV_GUID_CHILDREN,
                        0 | 1 => ::windows::Win32::System::Hypervisor::HV_GUID_LOOPBACK,
                        2 => ::windows::Win32::System::Hypervisor::HV_GUID_PARENT,
                        _ => ::windows::Win32::System::Hypervisor::HV_GUID_CHILDREN,
                    };
                    (g, port.to_guid())
                }
                VsockTarget::Guid(u) => (uuid_to_guid(u), port.to_guid()),
            };

            crate::transport::stream::vsock::windows::HvStream::connect(vm_guid, service_id).await
                .map(Self::Hv)
        }
    }
}

impl AsyncRead for VStream {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        match self {
            #[cfg(windows)]
            Self::Hv(s) => s.read(buf).await,

            #[cfg(unix)]
            Self::Vsock(s) => s.read(buf).await,
        }
    }
}

impl AsyncWrite for VStream {
    async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
        match self {
            #[cfg(windows)]
            Self::Hv(s) => s.write(buf).await,

            #[cfg(unix)]
            Self::Vsock(s) => s.write(buf).await,
        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::Hv(s) => s.flush().await,

            #[cfg(unix)]
            Self::Vsock(s) => s.flush().await,
        }
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::Hv(s) => s.shutdown().await,

            #[cfg(unix)]
            Self::Vsock(s) => s.shutdown().await,
        }
    }
}

pub enum VListener {
    #[cfg(windows)]
    Hv(crate::transport::stream::vsock::windows::HvListener),
    #[cfg(unix)]
    Vsock(crate::transport::stream::vsock::linux::VsockListener),
}

impl VListener {
    pub fn bind(target: VsockTarget, port: u32) -> io::Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self::Hv(
                crate::transport::stream::vsock::windows::HvListener::bind(target, port)?,
            ))
        }
        #[cfg(unix)]
        {
            Ok(Self::Vsock(
                crate::transport::stream::vsock::linux::VsockListener::bind(port)?,
            ))
        }
    }

    pub fn bind_loopback(port: u32) -> io::Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self::Hv(
                crate::transport::stream::vsock::windows::HvListener::bind(VsockTarget::Cid(0), port)?,
            ))
        }
        #[cfg(unix)] {
            Ok(Self::Vsock(
                crate::transport::stream::vsock::linux::VsockListener::bind_loopback(port)?,
            ))
        }
    }

    pub async fn accept(&self) -> io::Result<(VStream, SockAddr)> {
        match self {
            #[cfg(windows)]
            Self::Hv(l) => {
                let (stream, addr) = l.accept().await?;
                Ok((VStream::Hv(stream), addr))
            }
            #[cfg(unix)]
            Self::Vsock(l) => {
                let (stream, addr) = l.accept().await?;
                Ok((VStream::Vsock(stream), addr))
            },
        }
    }
}
