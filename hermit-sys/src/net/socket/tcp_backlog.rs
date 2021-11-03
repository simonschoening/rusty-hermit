use crate::net::socket::AsyncTcpSocket;
use crate::net::{nic, socket_map};
use futures_lite::future;
use hermit_abi::io;
use hermit_abi::net::event::EventFlags;
use hermit_abi::net::Socket;
use smoltcp::wire::IpEndpoint;
use std::task::{Poll, Waker};

#[derive(Debug, Clone)]
pub(crate) struct AsyncTcpBacklog {
	backlog: Vec<AsyncTcpSocket>,
	socket: Socket,
	local: IpEndpoint,
}

impl AsyncTcpBacklog {
	pub(crate) fn listen_on(async_socket: AsyncTcpSocket, capacity: usize) -> io::Result<Self> {
		let local = async_socket.local;
		let socket = async_socket.socket.ok_or(io::Error::new(
			io::ErrorKind::Other,
			"can't create tcp backlog from legacy socket",
		))?;
		let mut backlog = Vec::with_capacity(capacity);
		nic::lock().with(|nic| {
			for _ in 0..capacity {
				let handle = nic.create_tcp_handle();
				let socket = AsyncTcpSocket::new(Some(socket), handle, async_socket.local);
				backlog.push(socket);
			}
		});
		let mut slf = Self {
			backlog,
			socket,
			local,
		};
		for socket in slf.backlog.iter_mut() {
			socket.listen()?;
		}
		Ok(slf)
	}

	pub(crate) fn set_socket(&mut self, socket: Socket) {
		for async_socket in self.backlog.iter_mut() {
			async_socket.set_socket(socket);
		}
	}

	pub(crate) fn get_event_flags(&self) -> EventFlags {
		let mut flags = EventFlags::NONE;
		for socket in self.backlog.iter() {
			flags |= socket.get_event_flags().0;
		}
		EventFlags(flags)
	}

	pub(crate) fn register_exclusive_send_waker(&mut self, waker: &Waker) {
		for socket in self.backlog.iter_mut() {
			socket.register_exclusive_send_waker(waker);
		}
	}

	pub(crate) fn register_exclusive_recv_waker(&mut self, waker: &Waker) {
		for socket in self.backlog.iter_mut() {
			socket.register_exclusive_recv_waker(waker);
		}
	}

	pub(crate) fn accept(&self) -> io::Result<Socket> {
		let Self { socket, local, .. } = *self;
		self.backlog
			.iter()
            .enumerate()
			.find_map(|(index,async_socket)| 
                  async_socket.is_connected().then(|| index))
            .map(|index| {
                let handle = nic::lock().with(|nic| nic.create_tcp_handle());
                let mut new_socket = AsyncTcpSocket::new(Some(socket), handle, local);
                new_socket.listen()?;
                let mut socket_map = socket_map::lock();
                let backlog_entry = socket_map.get_mut(socket)?
                    .async_socket.get_tcp_backlog()?;
                let connected_socket = std::mem::replace(
                    &mut backlog_entry.backlog[index],
                    new_socket);
         
                socket_map
                    .get_mut(socket)
                    .map(|entry| {
                        entry.wake_tasks();
                        entry.options
                    })
                    .and_then(|options| {
                        let socket = socket_map.new_socket(options);
                        socket_map.bind_socket(socket, connected_socket.into())?;
                        Ok(socket)
                    })
			})
			.ok_or_else(|| {
                debug!("no connection available");
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "no socket in backlog is ready",
		    	)
            })?
	}

	pub(crate) fn close(&mut self) {
		for socket in self.backlog.iter_mut() {
			socket.rclose();
			socket.wclose();
		}
	}

	pub(crate) async fn wait_for_incoming_connection(&self) {
		future::poll_fn(|cx| {
            trace!("polling wait for incoming connection");
			if self.backlog.iter().any(AsyncTcpSocket::is_connected) {
				Poll::Ready(())
			} else if let Ok(entry) = socket_map::lock().get_mut(self.socket) {
			    entry.register_recv_waker(cx.waker());
				Poll::Pending
			} else {
                Poll::Ready(())
            }
		})
		.await
	}

	pub(crate) async fn wait_for_closed(&self) {
		for socket in self.backlog.iter() {
			socket.wait_for_closed().await;
		}
	}
}
