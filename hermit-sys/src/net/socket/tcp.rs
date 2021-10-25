use smoltcp::socket::{SocketHandle,TcpSocket,TcpState};
use smoltcp::wire::IpEndpoint;
use hermit_abi::io;
use hermit_abi::net::Socket;
use crate::net::socket_map;
use crate::net::nic;
use futures_lite::future;
use std::task::Poll;

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
enum WakeOn {
    Send,
    Recv,
    SendRecv,
    Any,
}

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
enum SocketState {
    Connecting,
    Connected,
    Closing,
    Closed,
}

/// Default keep alive interval in milliseconds
const DEFAULT_KEEP_ALIVE_INTERVAL: u64 = 75000;

#[derive(Debug,Clone)]
pub(crate) struct AsyncTcpSocket {
    handle: SocketHandle,
    // legacy sockets don't have a socket
    socket: Option<Socket>,
    state: SocketState,
}

impl AsyncTcpSocket {
    pub(crate) fn new(socket: Socket, handle: SocketHandle) -> Self {
        nic::lock().with(|nic| nic.socket_set.retain(handle));
        Self { 
            handle, 
            socket: Some(socket),  
            state: SocketState::Closed,
        }
    }

    pub(crate) fn remove(self) {
        if let Some(socket) = self.socket {
            socket_map::lock()
                .remove(socket);
        }
        nic::lock()
            .with(|nic| 
                nic
                    .socket_set
                    .release(self.handle));
    }

    async fn poll_with_socket<F,R>(&self,poll_f: F) -> R
    where
        F: Fn(&TcpSocket,&mut Option<WakeOn>) -> Poll<R>
    {
		future::poll_fn(|cx| {
            trace!("polling AsyncTcpSocket with {:?}", self.handle);
            let mut wake = None;
            // this will lock the nic
			let poll = self.with_socket_ref(|socket| 
                poll_f(socket, &mut wake));
            if let Poll::Pending = &poll {
                match wake { 
                    Some(wake @ WakeOn::Send) 
                    | Some(wake @ WakeOn::Recv) 
                    | Some(wake @ WakeOn::SendRecv) 
                    if self.socket.is_some() => {
                        let socket = self.socket.unwrap();
                        // when theres a waker to be registered 
                        // here we lock the socket_map (! after we unlocked the nic)
                        let mut sockets = socket_map::lock();
                        let entry = sockets.get_mut(socket).unwrap();
                        let waker = cx.waker();
                        if wake == WakeOn::SendRecv {
                            trace!("AsyncTcpSocket Future is Pending and wakes on send and recv");
                            entry.register_recv_waker(waker);
                            entry.register_send_waker(waker);
                        } else if wake == WakeOn::Recv {
                            trace!("AsyncTcpSocket Future is Pending and wakes on recv");
                            entry.register_recv_waker(waker);
                        } else if wake == WakeOn::Send {
                            trace!("AsyncTcpSocket Future is Pending and wakes on send");
                            entry.register_send_waker(waker);
                        }
                    },
                    Some(wake @ WakeOn::Send) 
                    | Some(wake @ WakeOn::Recv) 
                    | Some(wake @ WakeOn::SendRecv) => {
                        nic::lock().with(|nic| 
                            nic.with_tcp_socket_mut(self.handle,|tcp_socket| {
                                let waker = cx.waker();
                                if wake == WakeOn::SendRecv {
                                    tcp_socket.register_recv_waker(waker);
                                    tcp_socket.register_send_waker(waker);
                                } else if wake == WakeOn::Recv {
                                    tcp_socket.register_recv_waker(waker);
                                } else if wake == WakeOn::Send {
                                    tcp_socket.register_send_waker(waker);
                                }
                            })
                        );
                    },
                    Some(WakeOn::Any) => {
                        nic::register_waker(cx.waker());
                    },
                    None => {
                        unreachable!("No waker registered for Pending future! This is a Memory leak");
                    },
                }
            } else {
                trace!("AsyncTcpSocket poll ready");
            }
            poll
        }).await
    }

    /// call a closure with a reference to the corresponding socket in the nic
    ///
    /// this will lock the nic so don't aquire any other lock within the closure
	pub(crate) fn with_socket_ref<F,R>(&self, f: F) -> R 
    where
        F: FnOnce(&TcpSocket) -> R
    {
        nic::lock().with(|nic|
            nic.with_tcp_socket_ref(self.handle,f))
    }

    /// call a closure with a reference to the corresponding socket in the nic
    ///
    /// this will lock the nic so don't aquire any other lock within the closure
    /// this will wake the nic afterwards since the closure may alter the socket
    ///
    /// if the socket will not be modified use with_socket_mut instead to spare a wake
	pub(crate) fn with_socket_mut<F,R>(&mut self, f: F) -> R 
    where
        F: FnOnce(&mut TcpSocket) -> R
    {
        nic::lock().with(|nic| {
            let ret = nic.with_tcp_socket_mut(self.handle,f);
            nic.wake();
            ret
        })
    }

    /// Not in CLOSED, TIME-WAIT or LISTEN state
	pub(crate) fn is_active(&self) -> bool {
	    self.with_socket_ref( |socket| 
                socket.is_active())
    }

    /// Not in CLOSED or TIME-WAIT state
	pub(crate) fn is_open(&self) -> bool {
	    self.with_socket_ref( |socket| 
                socket.is_open())
    }

    /// In LISTEN state
	pub(crate) fn is_listening(&self) -> bool {
	    self.with_socket_ref( |socket| 
            if let TcpState::Listen = socket.state() {
                true
            } else {
                false
            })
    }

    /// In ESTABLISHED state
    pub(crate) fn is_connected(&self) -> bool {
        self.with_socket_ref(|socket| 
            if let TcpState::Established = socket.state() {
                true
            } else {
                false
            })
    }

    /// has data is receive buffer and may recv data
    pub(crate) fn can_recv(&self) -> bool {
        self.with_socket_ref(|socket| 
                socket.can_recv())
    }

    /// send buffer not full and can send
    pub(crate) fn can_send(&self) -> bool {
        self.with_socket_ref(|socket| 
                socket.can_send())
    }

	pub(crate) fn connect(&mut self, remote: IpEndpoint, local: IpEndpoint) -> io::Result<()> {
	    self.with_socket_mut(|socket| 
                socket.connect(remote, local))
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "can't connect. internal tcp error"))
    }

	pub(crate) fn listen(&mut self, local_addr: IpEndpoint) -> io::Result<()> {
        self.with_socket_mut(|socket| 
                socket.listen(local_addr))
            .map_err(|err| match err {
                smoltcp::Error::Illegal => io::Error::new(io::ErrorKind::Other, "already open"),
                smoltcp::Error::Unaddressable => io::Error::new(io::ErrorKind::InvalidInput, "port unspecified"),
                _ => io::Error::new(io::ErrorKind::Other, "unexpected internal error"),
            })
    }

	pub(crate) fn accept(&mut self) -> io::Result<IpEndpoint> {
		self.with_socket_mut(|socket| {
            socket
                .set_keep_alive(
                    Some(smoltcp::time::Duration::from_millis(DEFAULT_KEEP_ALIVE_INTERVAL)));
                Ok(socket.remote_endpoint())
        })
	}

    pub(crate) fn close(&mut self) {
		self.with_socket_mut(|socket|
            socket.close())
    }

	pub(crate) fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
		self.with_socket_mut(|socket| {
            if socket.can_recv() {
				socket.recv_slice(buffer).map_err(|_|
                    io::Error::new(io::ErrorKind::Other, "receive failed"))
            } else {
                Err(io::Error::new(io::ErrorKind::Other, "Can't receive"))
            }
        })
    }

	pub(crate) fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
		self.with_socket_mut(|socket| 
            if socket.can_send() {
				socket.send_slice(buffer).map_err(|_| 
                    io::Error::new(io::ErrorKind::Other, "send failed"))
			} else {
                Err(io::Error::new(io::ErrorKind::Other, "can't send"))
            }
		)
	}

	pub(crate) async fn wait_for_readable(&self) -> io::Result<()> {
        self.poll_with_socket(|socket,wake| {
            if !socket.may_recv() {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionAborted,"socket closed")))
            } else if socket.can_recv() {
                Poll::Ready(Ok(()))
            } else {
                wake.insert(WakeOn::Recv);
                Poll::Pending
            }
        }).await
	}

	pub(crate) async fn wait_for_writeable(&self) -> io::Result<()> {
        self.poll_with_socket(|socket,wake| {
            if !socket.may_send() {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionAborted,"socket closed")))
            } else if socket.can_send() {
                Poll::Ready(Ok(()))
            } else {
                wake.insert(WakeOn::Send);
                Poll::Pending
            }
		}).await
	}

    pub(crate) async fn wait_for_incoming_connection(&self) -> io::Result<()> {
        self.poll_with_socket(|socket,wake| {
            if !socket.is_open() {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionRefused,"socket closed")))
            } else if socket.is_active() {
                Poll::Ready(Ok(()))
            } else { // socket is still listening
                wake.insert(WakeOn::Send);
                Poll::Pending
            }
		}).await
    }

	pub(crate) async fn wait_for_connection(&self) -> io::Result<()> {
        self.poll_with_socket(|socket,wake| {
            if !socket.is_open() {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionRefused, "socket closed")))
            } else if let TcpState::Established = socket.state() {
                Poll::Ready(Ok(()))
            } else {
                wake.insert(WakeOn::Send);
                Poll::Pending
            }
        }).await
	}

    pub(crate) async fn wait_for_remaining_packets(&self) -> io::Result<()> {
        self.poll_with_socket(|socket,wake| match socket.state() {
            TcpState::Closed => Poll::Ready(
                Err(io::Error::new(io::ErrorKind::NotConnected,"socket already closed"))),
            TcpState::FinWait1
            | TcpState::FinWait2
            | TcpState::Closing
            | TcpState::TimeWait => Poll::Ready(
                Err(io::Error::new(io::ErrorKind::NotConnected,"socket already closing"))),
            _ => {
                if socket.send_queue() > 0 {
                    wake.insert(WakeOn::Send);
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(()))
                }
            }
        })
        .await
    }

	pub(crate) async fn wait_for_closed(&self) -> io::Result<()> {
        self.poll_with_socket(|socket,wake| match socket.state() {
            TcpState::Closed => Poll::Ready(Ok(())),
            state @ _ => {
                trace!("waiting to close AsyncTcpSocket with {:?} in state {:?}",self.handle,state);
                wake.insert(WakeOn::Any);
                Poll::Pending
            }
		})
		.await
	}
}

impl From<SocketHandle> for AsyncTcpSocket {
    fn from(handle: SocketHandle) -> Self {
        let mut socket = Self { 
            handle, 
            socket: None, 
            state: SocketState::Closed,
        };
        // from is used by the old api where the state is not preserved across function calls
        // therefore we need to check the state when creating the socket
        let active = socket.with_socket_ref(|tcp_socket| tcp_socket.is_active());
        if active {
            socket.state = SocketState::Connected;
        }
        socket
    }
}
