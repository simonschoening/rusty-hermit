use crate::io::Result;
use crate::net::event::{Event, EventFlags};
use crate::net::{Shutdown, Socket, SocketAddr};
use core::time::Duration;
use core::mem::MaybeUninit;

extern "Rust" {
	// Socket
	fn sys_socket() -> Result<Socket>;
	fn sys_socket_set_timeout(socket: Socket, timeout: Duration) -> Result<()>;
	fn sys_socket_set_non_blocking(socket: Socket, non_blocking: bool) -> Result<()>;
	fn sys_socket_close(socket: Socket) -> Result<()>;

	// Event
	fn sys_event_bind(socket: Socket) -> Result<()>;
	fn sys_event_add(socket: Socket, event: Event) -> Result<()>;
	fn sys_event_modify(socket: Socket, event: Event) -> Result<()>;
	fn sys_event_remove(socket: Socket, target: Socket) -> Result<()>;
	fn sys_event_wait(socket: Socket, events: &mut [MaybeUninit<Event>]) -> Result<usize>;

	// waker
	fn sys_waker_bind(socket: Socket) -> Result<()>;
	fn sys_waker_send_event(socket: Socket, flags: EventFlags) -> Result<()>;

	// TCP
	fn sys_tcp_bind(socket: Socket, local: SocketAddr) -> Result<()>;
	fn sys_tcp_listen(socket: Socket, backlog: usize) -> Result<()>;
	fn sys_tcp_accept(socket: Socket) -> Result<Socket>;
	fn sys_tcp_connect(socket: Socket, remote: SocketAddr) -> Result<()>;
	fn sys_tcp_shutdown(socket: Socket, mode: Shutdown) -> Result<()>;
	fn sys_tcp_read(socket: Socket, buf: &mut [u8]) -> Result<usize>;
	fn sys_tcp_write(socket: Socket, buf: &[u8]) -> Result<usize>;
}

// socket

pub fn socket() -> Result<Socket> {
	unsafe { sys_socket() }
}

pub fn socket_set_timeout(socket: Socket, timeout: Duration) -> Result<()> {
	unsafe { sys_socket_set_timeout(socket, timeout) }
}

pub fn socket_set_non_blocking(socket: Socket, non_blocking: bool) -> Result<()> {
	unsafe { sys_socket_set_non_blocking(socket, non_blocking) }
}

pub fn socket_close(socket: Socket) -> Result<()> {
	unsafe { sys_socket_close(socket) }
}

// event

pub fn event_bind(socket: Socket) -> Result<()> {
	unsafe { sys_event_bind(socket) }
}

pub fn event_add(socket: Socket, event: Event) -> Result<()> {
	unsafe { sys_event_add(socket, event) }
}

pub fn event_modify(socket: Socket, event: Event) -> Result<()> {
	unsafe { sys_event_modify(socket, event) }
}

pub fn event_remove(socket: Socket, target: Socket) -> Result<()> {
	unsafe { sys_event_remove(socket, target) }
}

pub fn event_wait(socket: Socket, events: &mut [MaybeUninit<Event>]) -> Result<usize> {
	unsafe { sys_event_wait(socket, events) }
}

// waker

pub fn waker_bind(socket: Socket) -> Result<()> {
	unsafe { sys_waker_bind(socket) }
}

pub fn waker_send_event(socket: Socket, flags: EventFlags) -> Result<()> {
	unsafe { sys_waker_send_event(socket, flags) }
}

// tcp

pub fn tcp_bind(socket: Socket, addr: SocketAddr) -> Result<()> {
	unsafe { sys_tcp_bind(socket, addr) }
}

pub fn tcp_listen(socket: Socket, backlog: usize) -> Result<()> {
	unsafe { sys_tcp_listen(socket, backlog) }
}

pub fn tcp_accept(socket: Socket) -> Result<Socket> {
	unsafe { sys_tcp_accept(socket) }
}

pub fn tcp_connect(socket: Socket, remote: SocketAddr) -> Result<()> {
	unsafe { sys_tcp_connect(socket, remote) }
}

pub fn tcp_shutdown(socket: Socket, mode: Shutdown) -> Result<()> {
	unsafe { sys_tcp_shutdown(socket, mode) }
}

pub fn tcp_read(socket: Socket, buf: &mut [u8]) -> Result<usize> {
	unsafe { sys_tcp_read(socket, buf) }
}

pub fn tcp_write(socket: Socket, buf: &[u8]) -> Result<usize> {
	unsafe { sys_tcp_write(socket, buf) }
}
