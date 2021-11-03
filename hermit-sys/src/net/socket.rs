use hermit_abi::io;
use hermit_abi::net::event::EventFlags;
use hermit_abi::net::Socket;
use std::task::Waker;

pub(crate) mod event;
pub(crate) mod tcp;
pub(crate) mod tcp_backlog;
pub(crate) mod waker;

pub(crate) use event::AsyncEventSocket;
pub(crate) use tcp::AsyncTcpSocket;
pub(crate) use tcp_backlog::AsyncTcpBacklog;
pub(crate) use waker::AsyncWakerSocket;

#[non_exhaustive]
#[derive(Debug, Clone)]
pub(crate) enum AsyncSocket {
	Unbound,
	Tcp(AsyncTcpSocket),
	TcpBacklog(AsyncTcpBacklog),
	Event(AsyncEventSocket),
	Waker(AsyncWakerSocket),
}

impl AsyncSocket {
	pub(crate) fn new() -> Self {
		Self::Unbound
	}

	pub(crate) fn set_socket(&mut self, socket: Socket) {
		match self {
			Self::Unbound => (),
			Self::Tcp(tcp) => tcp.set_socket(socket),
			Self::TcpBacklog(tcp_backlog) => tcp_backlog.set_socket(socket),
			Self::Event(event) => event.set_socket(socket),
			Self::Waker(waker) => waker.set_socket(socket),
		}
	}

	pub(crate) fn get_event_flags(&self) -> EventFlags {
		match self {
			Self::Unbound => EventFlags(EventFlags::NONE),
			Self::Tcp(tcp) => tcp.get_event_flags(),
			Self::TcpBacklog(tcp_backlog) => tcp_backlog.get_event_flags(),
			Self::Event(event) => event.get_event_flags(),
			Self::Waker(waker) => waker.get_event_flags(),
		}
	}

	pub(crate) fn register_exclusive_send_waker(&mut self, waker: &Waker) {
		match self {
			Self::Unbound => (),
			Self::Tcp(tcp) => tcp.register_exclusive_send_waker(waker),
			Self::TcpBacklog(tcp_backlog) => tcp_backlog.register_exclusive_send_waker(waker),
			Self::Event(_) => (),
			Self::Waker(wake) => wake.register_exclusive_send_waker(waker),
		}
	}

	pub(crate) fn register_exclusive_recv_waker(&mut self, waker: &Waker) {
		match self {
			Self::Unbound => (),
			Self::Tcp(tcp) => tcp.register_exclusive_recv_waker(waker),
			Self::TcpBacklog(tcp_backlog) => tcp_backlog.register_exclusive_recv_waker(waker),
			Self::Event(event) => event.register_exclusive_recv_waker(waker),
			Self::Waker(wake) => wake.register_exclusive_recv_waker(waker),
		}
	}

	pub(crate) fn close(&mut self) {
		match self {
			Self::Unbound => (),
			Self::Tcp(tcp) => tcp.close(),
			Self::TcpBacklog(tcp_backlog) => tcp_backlog.close(),
			Self::Event(_) => (),
			Self::Waker(waker) => waker.close(),
		}
	}

	pub(crate) async fn wait_for_closed(&self) {
		match self {
			Self::Unbound => (),
			Self::Tcp(tcp) => {
				tcp.wait_for_closed().await;
			}
			Self::TcpBacklog(tcp_backlog) => tcp_backlog.wait_for_closed().await,
			Self::Event(_) => (),
			Self::Waker(_) => (),
		}
	}

	// ---- tcp ----

	pub(crate) fn get_tcp(&mut self) -> io::Result<&mut AsyncTcpSocket> {
		if let AsyncSocket::Tcp(tcp) = self {
			Ok(tcp)
		} else {
			Err(io::Error::new(
				io::ErrorKind::InvalidData,
				"Not a tcp socket",
			))
		}
	}

	// ---- tcp backlog ----

	pub(crate) fn get_tcp_backlog(&mut self) -> io::Result<&mut AsyncTcpBacklog> {
		if let AsyncSocket::TcpBacklog(tcp) = self {
			Ok(tcp)
		} else {
			Err(io::Error::new(
				io::ErrorKind::InvalidData,
				"Not a tcp listener",
			))
		}
	}

	// ---- event ----

	pub(crate) fn get_event(&mut self) -> io::Result<&mut AsyncEventSocket> {
		if let AsyncSocket::Event(event) = self {
			Ok(event)
		} else {
			Err(io::Error::new(
				io::ErrorKind::InvalidData,
				"Not an event socket",
			))
		}
	}

	// ---- waker ----

	pub(crate) fn get_waker(&mut self) -> io::Result<&mut AsyncWakerSocket> {
		if let AsyncSocket::Waker(waker) = self {
			Ok(waker)
		} else {
			Err(io::Error::new(
				io::ErrorKind::InvalidData,
				"Not a waker socket",
			))
		}
	}
}

impl From<AsyncTcpSocket> for AsyncSocket {
	fn from(socket: AsyncTcpSocket) -> Self {
		Self::Tcp(socket)
	}
}

impl From<AsyncTcpBacklog> for AsyncSocket {
	fn from(backlog: AsyncTcpBacklog) -> Self {
		Self::TcpBacklog(backlog)
	}
}

impl From<AsyncEventSocket> for AsyncSocket {
	fn from(event: AsyncEventSocket) -> Self {
		Self::Event(event)
	}
}

impl From<AsyncWakerSocket> for AsyncSocket {
	fn from(waker: AsyncWakerSocket) -> Self {
		Self::Waker(waker)
	}
}
