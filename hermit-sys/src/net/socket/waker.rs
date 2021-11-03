use hermit_abi::net::event::EventFlags;
use hermit_abi::net::Socket;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::task::Waker;

#[derive(Debug, Clone)]
pub(crate) struct AsyncWakerSocket {
	socket: Option<Socket>,
	event_flags: Arc<AtomicU32>,
	send_waker: Option<Waker>,
	recv_waker: Option<Waker>,
}

impl AsyncWakerSocket {
	pub(crate) fn new(socket: Option<Socket>) -> Self {
		Self {
			socket,
			send_waker: None,
			recv_waker: None,
			event_flags: Arc::new(AtomicU32::new(EventFlags::NONE)),
		}
	}

	pub(crate) fn set_socket(&mut self, socket: Socket) {
		let _ = self.socket.insert(socket);
	}

	pub(crate) fn register_exclusive_send_waker(&mut self, waker: &Waker) {
		let _ = self.send_waker.insert(waker.clone());
	}

	pub(crate) fn register_exclusive_recv_waker(&mut self, waker: &Waker) {
		let _ = self.recv_waker.insert(waker.clone());
	}

	pub(crate) fn get_event_flags(&self) -> EventFlags {
		EventFlags(self.event_flags.swap(EventFlags::NONE, Ordering::SeqCst))
	}

	fn wake_send(&mut self) {
		if let Some(waker) = self.send_waker.take() {
			waker.wake();
		}
	}

	fn wake_recv(&mut self) {
		if let Some(waker) = self.recv_waker.take() {
			waker.wake();
		}
	}

	pub(crate) fn close(&mut self) {
		self.wake_send();
		self.wake_recv();
	}

	pub(crate) fn send_event(&mut self, event_flags: EventFlags) {
		if event_flags.0 & (EventFlags::RCLOSED | EventFlags::READABLE) != EventFlags::NONE {
			self.wake_recv();
		}
		if event_flags.0 & (EventFlags::RCLOSED | EventFlags::READABLE) != EventFlags::NONE {
			self.wake_recv();
		}
		self.event_flags.store(event_flags.0, Ordering::SeqCst);
	}
}
