use smoltcp::socket::{SocketHandle,TcpSocket,TcpState};
use smoltcp::wire::IpEndpoint;
use hermit_abi::io;
use hermit_abi::net::Socket;
use hermit_abi::net::event::EventFlags;
use crate::net::{socket_map,executor};
use crate::net::nic::{self,NetworkInterface};
use futures_lite::future;
use std::task::{Poll,Waker};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize,Ordering};

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
enum WakeOn {
    Send,
    Recv,
    SendRecv,
    Any,
}

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
#[repr(usize)]
enum State {
    /// currently listening
    Listening,
    /// currently connecting
    Connecting,
    /// Read and Write half connected
    Connected,
    /// read half closed by write half still connected
    RClosed,
    /// write half closed but read half still connected
    WClosed,
    /// currently closed
    ///
    /// contains an error and the state when it occured if is closed by insuccessful transaction
    Closed,
}

impl From<usize> for State {
    fn from(n: usize) -> Self {
        match n {
            0 => State::Listening,
            1 => State::Connecting,
            2 => State::Connected,
            3 => State::RClosed,
            4 => State::WClosed,
            5 => State::Closed,
            _ => panic!("unknown socket state"),
        }
    }
}

impl From<State> for usize {
    fn from(state: State) -> Self {
        state as Self
    }
}

#[derive(Debug,Clone)]
struct SharedState(Arc<AtomicUsize>);

impl SharedState {
    fn new(state: State) -> Self {
        Self(Arc::new(AtomicUsize::new(state.into())))
    }

    fn load(&self) -> State {
        self.0.load(Ordering::SeqCst).into()
    }

    fn store(&self, state: State) {
        self.0.store(state.into(), Ordering::SeqCst)
    }
}

/// Default keep alive interval in milliseconds
const DEFAULT_KEEP_ALIVE_INTERVAL: u64 = 75000;

#[derive(Debug)]
pub(crate) struct AsyncTcpSocket {
    /// handle to the smoltcp socket in the nic
    pub handle: SocketHandle,
    /// the bound local endpoint
    pub local: IpEndpoint,
    /// sockets waiting to gracefully close and legacy sockets don't have an entry in the socket_map 
    /// or associated state
    socket: Option<Socket>,
    /// this state is independent from the state of the connection and is driven by calls to the
    /// member functions taking a ref mut 
    state: SharedState,
    error: Option<io::Error>,
}

impl AsyncTcpSocket {
    pub(crate) fn new(socket: Option<Socket>, handle: SocketHandle, local: IpEndpoint) -> Self {
        Self { 
            handle,
            local,
            socket,
            state: SharedState::new(State::Closed),
            error: None,
        }
    }

    pub(crate) fn set_socket(&mut self, socket: Socket) {
        let _ = self.socket.insert(socket);
    }

    pub(crate) fn register_exclusive_send_waker(&mut self, waker: &Waker) {
        nic::lock().with(|nic| 
            nic.with_tcp_socket_mut(self.handle,|tcp_socket|
                tcp_socket.register_send_waker(waker)))
    }

    pub(crate) fn register_exclusive_recv_waker(&mut self, waker: &Waker) {
        nic::lock().with(|nic| 
            nic.with_tcp_socket_mut(self.handle,|tcp_socket|
                tcp_socket.register_recv_waker(waker)))
    }

    pub(crate) fn get_event_flags(&self) -> EventFlags {
        self.with_socket_ref(|socket| {
            let mut flags = EventFlags::NONE;
            if socket.can_send() {
                flags |= EventFlags::WRITABLE;
            } else if !socket.may_send() {
                flags |= EventFlags::WCLOSED;
            }
            if socket.can_recv() {
                flags |= EventFlags::READABLE;
            } else if !socket.may_recv() {
                flags |= EventFlags::WCLOSED;
            }
            EventFlags(flags)
        })
    }

    fn update_state(&mut self) -> State {
        let (new_state,error) = match self.state.load() {
            state @ State::Closed => (state,None),
            state @ State::Connecting => self.with_socket_ref(|socket| {
                if socket.may_send() && socket.may_recv() {
                    (State::Connected,None)
                } else if !socket.is_open() {
                    debug!("conection refused, socket in state {:?}", self.with_socket_ref(|tcp| tcp.state()));
                    (State::Closed,Some(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "socket closed while connecting",
                    )))
                } else {
                    (state,None)
                }
            }),
            state @ State::Connected => self.with_socket_ref(|socket| {
                if !socket.is_open() {
                    (State::Closed,Some(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "socket closed",
                    )))
                } else if socket.may_send() && !socket.may_recv() {
                    (State::RClosed,None)
                } else if socket.may_recv() && !socket.may_send() {
                    (State::WClosed,None)
                } else {
                    (state,None)
                }
            }),
            state @ State::RClosed => self.with_socket_ref(|socket| {
                if !socket.is_open() {
                    (State::Closed,None)
                } else {
                    (state,None)
                }
            }),
            state @ State::WClosed => self.with_socket_ref(|socket| {
                if !socket.is_open() {
                    (State::Closed,None)
                } else {
                    (state,None)
                }
            }),
            state @ State::Listening => self.with_socket_ref(|socket| {
                if socket.may_send() && socket.may_recv() {
                    (State::Connected,None)
                } else if !socket.is_open() {
                    (State::Closed,Some(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "socket closed",
                    )))
                } else {
                    (state,None)
                }
            }),
        };
        self.error = error;
        self.state.store(new_state);
        new_state
    }

    /// poll a function/closure with a socket to await a specific condition on the underlying smoltcp socket
    async fn poll_with_socket<F>(&self,poll_f: F)
    where
        F: Fn(&TcpSocket,&mut Option<WakeOn>) -> Poll<()>
    {
		future::poll_fn(|cx| {
            trace!("polling AsyncTcpSocket with {:?}", self.handle);
            let mut wake = None;
            // this will lock the nic and poll the future with the socket
			let poll = self.with_socket_ref(|socket| 
                poll_f(socket, &mut wake));
            // if Poll is Pending it must have indicated when to wake
            if let Poll::Pending = &poll {
                match wake { 
                    Some(wake @ WakeOn::Send) 
                    | Some(wake @ WakeOn::Recv) 
                    | Some(wake @ WakeOn::SendRecv) => {
                        if let Some(socket) = self.socket {
                            // socket is registered in socket_map so register the waker there
                            let mut sockets = socket_map::lock();
                            if let Ok(entry) = sockets.get_mut(socket) {
                                let waker = cx.waker();
                                if wake == WakeOn::Recv || wake == WakeOn::SendRecv {
                                    trace!("AsyncTcpSocket Future is Pending and wakes on recv");
                                    entry.register_recv_waker(waker);
                                }
                                if wake == WakeOn::Send || wake == WakeOn::SendRecv {
                                    trace!("AsyncTcpSocket Future is Pending and wakes on send");
                                    entry.register_send_waker(waker);
                                }
                            } else {
                                return Poll::Ready(());
                            }
                        } else {
                            nic::lock().with(|nic| 
                                nic.with_tcp_socket_mut(self.handle,|tcp_socket| {
                                    let waker = cx.waker();
                                    if wake == WakeOn::Recv || wake == WakeOn::SendRecv {
                                        trace!("AsyncTcpSocket Future is Pending and wakes on recv");
                                        tcp_socket.register_recv_waker(waker);
                                    }
                                    if wake == WakeOn::Send || wake == WakeOn::SendRecv {
                                        trace!("AsyncTcpSocket Future is Pending and wakes on send");
                                        tcp_socket.register_send_waker(waker);
                                    }
                                })
                            );
                        }
                    },
                    Some(WakeOn::Any) => {
                        nic::register_waker(cx.waker());
                    },
                    None => {
                        unreachable!("No waker registered for Pending future! This is at least Memory leak or worse a deadlock");
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
	    self.with_socket_ref(|socket|
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
            socket.is_listening())
    }

    /// In ESTABLISHED state
    pub(crate) fn is_connected(&self) -> bool {
        self.with_socket_ref(|socket| 
            socket.state() == TcpState::Established)
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

    /// connect this socket to a remote using the local address
    ///
    /// the socket must not be open
	pub(crate) fn connect(&mut self, remote: IpEndpoint, local: IpEndpoint) -> io::Result<()> {
        if !remote.is_specified() || remote.port == 0 {
            Err(io::Error::new(io::ErrorKind::InvalidData, "can't connect. invalid remote"))
        } else if local.port == 0 {
            Err(io::Error::new(io::ErrorKind::InvalidData, "can't connect. invalid local"))
        } else {
            match self.update_state() {
                State::Closed if self.is_open() => {
                    Err(io::Error::new(
                        io::ErrorKind::WouldBlock, 
                        "the socket is still closing"
                    ))
                }
                // this socket can by connected
                State::Closed if self.error.is_none()  => {
                    self.with_socket_mut(|socket| 
                            socket.connect(remote, local))
                        .map_err(|_| io::Error::new(io::ErrorKind::Other, "can't connect. internal tcp error"))
                        .map(|()| self.state.store(State::Connecting))
                },
                // closed by error
                State::Closed => Err(self.error.take().unwrap()),
                // already connecting
                State::Connecting => {
                    nic::lock().with(NetworkInterface::wake);
                    Err(io::Error::new(
                        io::ErrorKind::WouldBlock, 
                        "the socket is aready connecting to some address"
                    ))
                },
                // connected => when endpoints match consider it successful
                State::Connected => 
                    if self.with_socket_ref(|socket| socket.remote_endpoint() == remote) {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::InUse, 
                            "this socket is aready connected to another address",
                        ))
                    },
                _ =>
                    Err(io::Error::new(
                        io::ErrorKind::InUse, 
                        "this socket is aready in use",
                    )),
            }
        }
    }

    /// listen on this socket for ONE incoming connection
    /// if the backlog behaviour of berkley sockets is desired an AsyncTcpBacklog should be used
	pub(crate) fn listen(&mut self, local_addr: IpEndpoint) -> io::Result<()> {
        match self.update_state() {
            State::Closed if self.is_open() => return
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock, 
                    "the socket is still closing"
                )),
            State::Closed => (),
            _ => return
                Err(io::Error::new(
                    io::ErrorKind::InUse, 
                    "this socket is aready in use",
                )),
        }
        self.with_socket_mut(|socket| 
                socket.listen(local_addr))
            .map_err(|err| match err {
                smoltcp::Error::Illegal => io::Error::new(io::ErrorKind::Other, "already open"),
                smoltcp::Error::Unaddressable => io::Error::new(io::ErrorKind::InvalidInput, "port unspecified"),
                _ => io::Error::new(io::ErrorKind::Other, "unexpected internal error"),
            })
            .map(|()| self.state.store(State::Listening))
    }

	pub(crate) fn accept(&mut self) -> io::Result<IpEndpoint> {
        match self.update_state() {
            State::Connected =>
                self.with_socket_mut(|socket| {
                    socket
                        .set_keep_alive(
                            Some(smoltcp::time::Duration::from_millis(DEFAULT_KEEP_ALIVE_INTERVAL)));
                        Ok(socket.remote_endpoint())
                }),
            State::Closed if self.error.is_some() =>  Err(self.error.take().unwrap()),
            State::Closed => 
                Err(io::Error::new(
                    io::ErrorKind::NotListening, 
                    "the socket is not listening"
                )),
            State::Listening => 
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock, 
                    "the socket is still listening"
                )),
            _ => 
                Err(io::Error::new(
                    io::ErrorKind::InUse, 
                    "the socket is not a listener"
                )),
        }
	}

    pub(crate) fn wclose(&mut self) {
        let new_state = match self.update_state() { 
            State::Closed | State::RClosed => State::Closed,
            _ => State::WClosed,
        };
        self.state.store(new_state);
        let mut async_socket = self.clone();
        executor::spawn( async move {
            async_socket.wait_for_remaining_packets().await;
            trace!("graceful shutdown completed, closing socket");
		    async_socket.with_socket_mut(|socket|
                socket.close());
        }).detach();
    }

    pub(crate) fn rclose(&mut self) {
        let new_state = match self.update_state() { 
            State::Closed | State::WClosed => State::Closed,
            _ => State::RClosed,
        };
        self.state.store(new_state);
    }

    pub(crate) fn close(&mut self) {
		self.with_socket_mut(|socket|
            socket.abort());
        self.state.store(State::Closed);
    }

	pub(crate) fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.update_state() {
            State::Connected | State::WClosed =>
                self.with_socket_mut(|socket| {
                    if socket.can_recv() {
                        socket.recv_slice(buffer).map_err(|_|
                            io::Error::new(io::ErrorKind::Other, "receive failed"))
                    } else {
                        Err(io::Error::new(io::ErrorKind::Other, "Can't receive"))
                    }
                }),
            _ => 
                Err(io::Error::new(
                    io::ErrorKind::NotConnected, 
                    "the sockets read side is closed"
                )),
        }
    }

	pub(crate) fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self.update_state() {
            State::Connected | State::RClosed =>
                self.with_socket_mut(|socket| {
                    if socket.can_send() {
                        socket.send_slice(buffer).map_err(|_| 
                            io::Error::new(io::ErrorKind::Other, "send failed"))
                    } else {
                        Err(io::Error::new(io::ErrorKind::Other, "can't send"))
                    }
                }),
            _ => 
                Err(io::Error::new(
                    io::ErrorKind::NotConnected, 
                    "the sockets read side is closed"
                )),
        }
	}

	pub(crate) async fn wait_for_readable(&self) {
        self.poll_with_socket(|socket,wake| {
            if !socket.may_recv() {
                Poll::Ready(())
            } else if socket.can_recv() {
                Poll::Ready(())
            } else {
                let _ = wake.insert(WakeOn::Recv);
                Poll::Pending
            }
        }).await
	}

	pub(crate) async fn wait_for_writeable(&self) {
        self.poll_with_socket(|socket,wake| {
            if !socket.may_send() {
                Poll::Ready(())
            } else if socket.can_send() {
                Poll::Ready(())
            } else {
                let _ = wake.insert(WakeOn::Send);
                Poll::Pending
            }
		}).await
	}

    pub(crate) async fn wait_for_incoming_connection(&self) {
        self.poll_with_socket(|socket,wake| {
            if !socket.is_open() {
                Poll::Ready(())
            } else if socket.is_active() {
                Poll::Ready(())
            } else {
                let _ = wake.insert(WakeOn::Recv);
                Poll::Pending
            }
		}).await
    }

	pub(crate) async fn wait_for_connection(&self) {
        self.poll_with_socket(|socket,wake| {
            if !socket.is_open() {
                Poll::Ready(())
            } else if socket.may_send() {
                Poll::Ready(())
            } else {
                trace!("pending while waiting for connection");
                let _ = wake.insert(WakeOn::Recv);
                Poll::Pending
            }
        }).await
	}

    pub(crate) async fn wait_for_remaining_packets(&self) {
        self.poll_with_socket(|socket,wake| {
            trace!("waiting for remaining packets of {:?}",self);
            if socket.may_send() && socket.send_queue() > 0 {
                let _ = wake.insert(WakeOn::Send);
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await
    }

	pub(crate) async fn wait_for_closed(&self) {
        self.poll_with_socket(|socket,wake| match socket.state() {
            TcpState::Closed => Poll::Ready(()),
            state @ _ => {
                trace!("waiting to close AsyncTcpSocket with {:?} in state {:?}",self.handle,state);
                let _ = wake.insert(WakeOn::Any);
                Poll::Pending
            }
		})
		.await
	}
}

impl From<SocketHandle> for AsyncTcpSocket {
    fn from(handle: SocketHandle) -> Self {
        let is_active = nic::lock().with(|nic| {
            nic.socket_set.retain(handle);
            nic.with_tcp_socket_ref(handle,|tcp| tcp.is_active())
        });
        let socket = Self { 
            handle, 
            socket: None,
            local: IpEndpoint::UNSPECIFIED,
            state: SharedState::new(if is_active { State::Connected } else { State::Closed }),
            error: None,
        };
        socket
    }
}

impl Clone for AsyncTcpSocket {
    fn clone(&self) -> Self {
        nic::lock().with(|nic| nic.socket_set.retain(self.handle));
        Self { state: self.state.clone(), ..*self }
    }
}

impl Drop for AsyncTcpSocket {
    fn drop(&mut self) {
        debug!("dropping async socket");
        nic::lock().with(|nic| nic.socket_set.release(self.handle));
    }
}
