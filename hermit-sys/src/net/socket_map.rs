use futures_lite::future;
use smoltcp::time::Duration;
use std::sync::{Mutex, MutexGuard};
use std::task::{Poll, Waker};

use hermit_abi::io;
use hermit_abi::net::Socket;

use crate::net::executor;
use crate::net::socket::AsyncSocket;

lazy_static! {
	static ref SOCKETS: Mutex<SocketMap> = Mutex::new(SocketMap::new());
}

pub(crate) fn lock() -> MutexGuard<'static, SocketMap> {
	SOCKETS.lock().expect("SocketMap mutex poisoned")
}

/// this struct provides conversion from the exposed abi::Socket
/// to the internal entry
#[derive(Debug)]
pub(crate) struct SocketMap {
	sockets: Vec<Option<SocketEntry>>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
	pub non_blocking: bool,
	pub timeout: Option<Duration>,
}

/// the socket_entry manages options related to calling behaviour (like non_blocking),
/// to access settings for lower levels or query things about the socket
/// the async_socket within is used
#[derive(Debug)]
pub(crate) struct SocketEntry {
	/// the async socket to perform operations on
	pub async_socket: AsyncSocket,
	/// options for callign behaviour of a socket
	pub options: Options,
	// this task registers it's waker on the socket
	// and on wake drains and wakes all recv_wakers
	// it must be woken when the underlying socket changes
	recv_task_waker: Option<Waker>,
	// this task registers it's waker on the socket
	// and on wake drains and wakes all send_wakers
	// it must be woken when the underlying socket changes
	send_task_waker: Option<Waker>,
	// all registered recv wakers
	recv_wakers: Vec<Waker>,
	// all registered send wakers
	send_wakers: Vec<Waker>,
}

impl SocketEntry {
	/// split the entry into an async_socket and it's options
	pub(crate) fn split(self) -> (AsyncSocket, Options) {
		(self.async_socket, self.options)
	}

	/// split the entry into an async_socket and it's options
	pub(crate) fn split_cloned(&self) -> (AsyncSocket, Options) {
		(self.async_socket.clone(), self.options.clone())
	}

	/// split the entry into references to an async_socket and it's options
	pub(crate) fn split_ref(&self) -> (&AsyncSocket, &Options) {
		(&self.async_socket, &self.options)
	}

	/// split the entry into mutable references to an async_socket and it's options
	pub(crate) fn split_mut(&mut self) -> (&mut AsyncSocket, &mut Options) {
		(&mut self.async_socket, &mut self.options)
	}

	/// register a send waker
	///
	/// this is safe to call for different wakers
	pub(crate) fn register_send_waker(&mut self, waker: &Waker) {
		self.send_wakers.push(waker.clone());
	}

	/// register a recv waker
	///
	/// this is safe to call for different wakers
	pub(crate) fn register_recv_waker(&mut self, waker: &Waker) {
		self.recv_wakers.push(waker.clone());
	}

	/// wake the associated tasks to notify them of changes to this socket
	pub(crate) fn wake_tasks(&mut self) {
		if let Some(waker) = self.send_task_waker.take() {
			waker.wake();
		}
		if let Some(waker) = self.recv_task_waker.take() {
			waker.wake();
		}
	}

	/// wake all registered recv wakers
	pub(crate) fn wake_recv(&mut self) {
		trace!("waking recv for {:?}", self.async_socket);
		for waker in self.recv_wakers.drain(..) {
			waker.wake();
		}
	}

	/// wake all registered recv wakers
	pub(crate) fn wake_send(&mut self) {
		trace!("waking send for {:?}", self.async_socket);
		for waker in self.send_wakers.drain(..) {
			waker.wake();
		}
	}
}

impl SocketMap {
	/// create a new SocketMap
	fn new() -> Self {
		Self {
			sockets: Vec::with_capacity(256),
		}
	}

	/// get a vacant entry or allocate one if none exist
	fn next_free_entry(&mut self) -> Socket {
		// find a vacant slot or create a new one
		let id = self
			.sockets
			.iter()
			.enumerate()
			.find_map(|(id, entry)| entry.is_none().then(|| id))
			.unwrap_or_else(|| {
				self.sockets.push(None);
				self.sockets.len() - 1
			});
		Socket { id }
	}

	/// close a socket by removing it's entry
	/// cancelling the wake futures
	pub(crate) fn take(&mut self, socket: Socket) -> io::Result<SocketEntry> {
		self.sockets
			.get_mut(socket.id)
			.and_then(Option::take)
			.map(|mut entry| entry)
			.ok_or(io::Error::new(io::ErrorKind::NotFound, "unknown socket"))
	}

	pub(crate) fn get(&self, socket: Socket) -> io::Result<&SocketEntry> {
		self.sockets
			.get(socket.id)
			.and_then(Option::as_ref)
			.ok_or(io::Error::new(io::ErrorKind::NotFound, "unknown socket"))
	}

	pub(crate) fn get_mut(&mut self, socket: Socket) -> io::Result<&mut SocketEntry> {
		self.sockets
			.get_mut(socket.id)
			.and_then(Option::as_mut)
			.ok_or(io::Error::new(io::ErrorKind::NotFound, "unknown socket"))
	}

	/// insert an async socket and update daemon futures
	pub(crate) fn bind_socket(
		&mut self,
		socket: Socket,
		async_socket: AsyncSocket,
	) -> io::Result<()> {
		let entry = self.get_mut(socket)?;
		entry.async_socket = async_socket;
		entry.async_socket.set_socket(socket);
		entry.wake_tasks();
		entry.wake_send();
		entry.wake_recv();
		Ok(())
	}

	pub(crate) fn new_socket(&mut self, options: Options) -> Socket {
		// get a free entry
		let socket = self.next_free_entry();
		// spawn the waker futures for this entry
		executor::spawn(future::poll_fn(move |cx| {
			let mut sockets = lock();
			if let Ok(entry) = sockets.get_mut(socket) {
				let _ = entry.send_task_waker.insert(cx.waker().clone());
				entry.wake_send();
				let mut async_socket = entry.async_socket.clone();
				drop(sockets);
				async_socket.register_exclusive_send_waker(cx.waker());
				Poll::Pending
			} else {
				Poll::Ready(())
			}
		}))
		.detach();
		executor::spawn(future::poll_fn(move |cx| {
			let mut sockets = lock();
			if let Ok(entry) = sockets.get_mut(socket) {
				let _ = entry.send_task_waker.insert(cx.waker().clone());
				entry.wake_recv();
				let mut async_socket = entry.async_socket.clone();
				drop(sockets);
				async_socket.register_exclusive_recv_waker(cx.waker());
				Poll::Pending
			} else {
				Poll::Ready(())
			}
		}))
		.detach();
		// insert the socket
		let _ = self.sockets[socket.id].insert(SocketEntry {
			async_socket: AsyncSocket::Unbound,
			options,
			send_task_waker: None,
			recv_task_waker: None,
			send_wakers: Vec::with_capacity(4),
			recv_wakers: Vec::with_capacity(4),
		});
		socket
	}
}
