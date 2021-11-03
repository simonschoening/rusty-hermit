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

	pub(crate) fn accept(&mut self) -> io::Result<Socket> {
		let Self { socket, local, .. } = *self;
		self.backlog
			.iter_mut()
			.find_map(|async_socket| {
				if let Ok(_) = async_socket.accept() {
					let handle = nic::lock().with(|nic| nic.create_tcp_handle());
					let async_socket = std::mem::replace(
						async_socket,
						AsyncTcpSocket::new(Some(socket), handle, local),
					);
					let mut socket_map = socket_map::lock();
					Some(
						socket_map
							.get_mut(socket)
							.map(|entry| {
								entry.wake_tasks();
								entry.options
							})
							.and_then(|options| {
								let socket = socket_map.new_socket(options);
								socket_map.bind_socket(socket, async_socket.into())?;
								Ok(socket)
							}),
					)
				} else {
					None
				}
			})
			.ok_or(io::Error::new(
				io::ErrorKind::WouldBlock,
				"no socket in backlog is ready",
			))?
	}

	pub(crate) fn close(&mut self) {
		for socket in self.backlog.iter_mut() {
			socket.close();
		}
	}

	pub(crate) async fn wait_for_incoming_connection(&self) {
		future::poll_fn(|cx| {
			if self.backlog.iter().any(AsyncTcpSocket::is_connected) {
				Poll::Ready(())
			} else {
				socket_map::lock()
					.get_mut(self.socket)
					.unwrap()
					.register_send_waker(cx.waker());
				Poll::Pending
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
