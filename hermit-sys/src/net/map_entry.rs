use smoltcp::socket::SocketHandle;
use hermit_abi::io;
use hermit_abi::net;

#[derive(Clone)]
pub enum Inner {
    Handle(SocketHandle),
    Backlog(Vec<SocketHandle>),
}

#[derive(Clone)]
pub(crate) struct MapEntry {
    pub inner: Inner,
    pub info: net::SocketInfo,
    pub ref_count: usize,
}

impl MapEntry {
    pub fn add_tcp_backlog(&mut self, Vec<SocketHandle>) {
        
    }

    pub fn as_tcp_handle_mut(&self) -> io::Result<(SocketHandle,net::SocketInfo)> {
        if let net::SocketType::Tcp = self.info.socket_type {
            if let Inner::Handle(handle) = self.inner {
                return Ok((handle,self.info));
            } else {
                return Err(io::Error {
                    kind: io::ErrorKind::InvalidInput,
                    msg: "socket is listening",
                });
            }
        } else {
            return Err(io::Error {
                kind: io::ErrorKind::InvalidInput,
                msg: "socket is not a tcp socket",
            });
        }
    }

    pub fn as_tcp_backlog_mut(&mut self) 
    -> io::Result<(&mut Vec<SocketHandle>, &mut net::SocketInfo)> 
    {
        let Self { ref mut inner, ref mut info, .. } = self;
        if let net::SocketType::Tcp = info.socket_type {
            if let Inner::Backlog(ref mut handles) = inner {
                return Ok((handles,info));
            } else {
                return Err(io::Error {
                    kind: io::ErrorKind::InvalidInput,
                    msg: "socket is not listening",
                });
            }
        } else {
            return Err(io::Error {
                kind: io::ErrorKind::InvalidInput,
                msg: "socket is not a tcp socket",
            });
        }
    }
}
