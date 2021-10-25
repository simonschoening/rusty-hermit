use hermit_abi::io;

pub(crate) mod tcp;
pub(crate) use tcp::AsyncTcpSocket;

#[non_exhaustive]
#[derive(Debug,Clone)]
pub(crate) enum AsyncSocket {
    Tcp(AsyncTcpSocket),
//    TcpBacklog(AsyncTcpBacklog),
}

impl AsyncSocket {
    pub(crate) fn get_tcp(&self) -> io::Result<AsyncTcpSocket> {
        if let AsyncSocket::Tcp(ref tcp) = self {
            Ok(tcp.clone())
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidData,"Not a tcp socket"))
        }
    }

    pub(crate) fn as_tcp_ref(&self) -> io::Result<&AsyncTcpSocket> {
        if let AsyncSocket::Tcp(ref tcp) = self {
            Ok(tcp)
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidData,"Not a tcp socket"))
        }
    }

    pub(crate) fn as_tcp_mut(&mut self) -> io::Result<&mut AsyncTcpSocket> {
        if let AsyncSocket::Tcp(ref mut tcp) = self {
            Ok(tcp)
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidData,"Not a tcp socket"))
        }
    }
}

impl From<AsyncTcpSocket> for AsyncSocket {
    fn from(async_socket: AsyncTcpSocket) -> Self {
        Self::Tcp(async_socket)
    }
}
/*
impl From<AsyncTcpBacklog> for AsyncSocket {
    fn from(async_socket: AsyncTcpSocket) -> Self {
        Self::Tcp(async_socket)
    }
}*/
