pub mod device;
pub mod executor;
pub mod nic;
pub mod socket;
pub mod socket_map;
pub mod waker;

#[cfg(target_arch = "aarch64")]
use aarch64::regs::*;
use hermit_abi::net::event::{Event, EventFlags};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::_rdtsc;
use std::convert::TryInto;
use std::mem::MaybeUninit;
use std::str::FromStr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::task::Poll;
use std::time::Duration;
use std::u16;

mod smol {
	#[cfg(feature = "dhcpv4")]
	pub use smoltcp::dhcp::Dhcpv4Client;
	#[cfg(feature = "trace")]
	pub use smoltcp::phy::EthernetTracer;
	#[cfg(feature = "dhcpv4")]
	pub use smoltcp::wire::{IpCidr, Ipv4Address, Ipv4Cidr};

	pub use smoltcp::iface::EthernetInterface;
	pub use smoltcp::phy::Device;
	pub use smoltcp::socket::{
		Socket, SocketHandle, SocketSet, TcpSocket, TcpSocketBuffer, TcpState,
	};
	pub use smoltcp::time::{Duration, Instant};
	pub use smoltcp::wire::{IpAddress, IpEndpoint};
	pub use smoltcp::Error;
}

use hermit_abi::io;
use hermit_abi::net as abi;
use hermit_abi::Tid;

use futures_lite::future;

use crate::net::device::HermitNet;
use crate::net::executor::{block_on, poll_on, run_executor, spawn};
use crate::net::socket::AsyncTcpSocket;

macro_rules! abi_to_smol {
    ($expr:expr => IpEndpoint) => {
        match $expr {
            abi::SocketAddr::V4(addr) => {
                let ip = abi_to_smol!(addr.ip_addr => Ipv4Address);
                smol::IpEndpoint {
                    addr: ip,
                    port: addr.port
                }
            },
            abi::SocketAddr::V6(addr) => {
                let ip = abi_to_smol!(addr.ip_addr => Ipv6Address);
                smol::IpEndpoint {
                    addr: ip,
                    port: addr.port
                }
            },
        }
    };
    ($expr:expr => IpAddress) => {
        match $expr {
            abi::IpAddr::V4(ip) => abi_to_smol!(ip => Ipv4Address),
            abi::IpAddr::V6(ip) => abi_to_smol!(ip => Ipv6Address),
        }
    };
    ($expr:expr => Ipv4Address) => {
        {
            let ip = $expr;
            smol::IpAddress::v4(ip.a ,ip.b ,ip.c ,ip.d)
        }
    };
    ($expr:expr => Ipv6Address) => {
        {
            let ip = $expr;
            smol::IpAddress::v6(ip.a, ip.b, ip.c, ip.d, ip.e, ip.f, ip.g, ip.h)
        }
    }
}

// reduce boilerplate and convieniently use hermit_abi::io:Error everywhere
macro_rules! IOError {
	($kind:ident, $msg:literal) => {
		io::Error {
			kind: hermit_abi::io::ErrorKind::$kind,
			msg: $msg,
		}
	};
}

extern "C" {
	fn sys_yield();
	fn sys_spawn(
		id: *mut Tid,
		func: extern "C" fn(usize),
		arg: usize,
		prio: u8,
		selector: isize,
	) -> i32;
	fn sys_netwait();
}

static LOCAL_ENDPOINT: AtomicU16 = AtomicU16::new(0);

fn local_endpoint() -> u16 {
	LOCAL_ENDPOINT.fetch_add(1, Ordering::Acquire) | 0xC000
}

#[cfg(target_arch = "x86_64")]
fn start_endpoint() -> u16 {
	((unsafe { _rdtsc() as u64 }) % (u16::MAX as u64))
		.try_into()
		.unwrap()
}

#[cfg(target_arch = "aarch64")]
fn start_endpoint() -> u16 {
	(CNTPCT_EL0.get() % (u16::MAX as u64)).try_into().unwrap()
}

pub(crate) fn network_delay(timestamp: smol::Instant) -> Option<smol::Duration> {
	nic::lock().with(|nic| nic.poll_delay(timestamp))
}

pub(crate) async fn network_run() {
	let _: () = future::poll_fn(|cx| {
		trace!("network_run");
		nic::lock().with(|nic| {
			nic.socket_set.prune();
			nic.poll(cx, smol::Instant::now());
		});
		Poll::Pending
	})
	.await;
}

extern "C" fn nic_thread(_: usize) {
	loop {
		unsafe {
			sys_netwait();
		}

		// wake the network:run future on the executor
		trace!("waking nic");
		nic::lock().with(|nic| nic.wake());
		// this wakes all thread that have blocking io ready
		run_executor();
		trace!("nic_thread ready");
	}
}

pub(crate) fn network_init() -> io::Result<()> {
	// initialize variable, which contains the next local endpoint
	LOCAL_ENDPOINT.store(start_endpoint(), Ordering::Release);

	let mut guard = nic::lock();
	*guard = nic::NetworkInterface::<HermitNet>::new();

	guard
		.with(|nic| Ok(nic.poll_delay(smoltcp::time::Instant::now())))
		.and_then(|_| {
			spawn(network_run()).detach();

			// create thread, which manages the network stack
			// use a higher priority to reduce the network latency
			let mut tid: Tid = 0;
			let ret = unsafe { sys_spawn(&mut tid, nic_thread, 0, 3, 0) };
			if ret >= 0 {
				debug!("Spawn network thread with id {}", tid);
			}

			// switch to network thread
			unsafe { sys_yield() };
			Ok(())
		})
}

///////////////////////////////////////////////////////////////////
// new interface
///////////////////////////////////////////////////////////////////

/// creates a new socket (TCP/UDP/...)
#[no_mangle]
pub fn sys_socket() -> io::Result<abi::Socket> {
	let socket = socket_map::lock().new_socket(socket_map::Options {
		non_blocking: false,
		timeout: None,
	});
	run_executor();
	Ok(socket)
}

/// makes the socket non_blocking
#[no_mangle]
pub fn sys_socket_set_timeout(socket: abi::Socket, timeout: Option<Duration>) -> io::Result<()> {
	socket_map::lock().get_mut(socket)?.options.timeout =
		timeout.map(|duration| smol::Duration::from(duration));
	Ok(())
}

/// makes the socket non_blocking
#[no_mangle]
pub fn sys_socket_set_non_blocking(socket: abi::Socket, non_blocking: bool) -> io::Result<()> {
	socket_map::lock().get_mut(socket)?.options.non_blocking = non_blocking;
	Ok(())
}

/// close a socket
#[no_mangle]
pub fn sys_socket_close(socket: abi::Socket) -> io::Result<()> {
	run_executor();
	let (mut async_socket, options) = socket_map::lock().take(socket)?.split();
	async_socket.close();
	if !options.non_blocking {
		block_on(async_socket.wait_for_closed(), None)?;
	}
	Ok(())
}

// ---- event ----

#[no_mangle]
pub fn sys_event_bind(socket: abi::Socket) -> io::Result<()> {
	debug!("binding event socket");
	socket_map::lock().bind_socket(socket, socket::AsyncEventSocket::new(None).into())?;
	run_executor();
	Ok(())
}

#[no_mangle]
pub fn sys_event_add(socket: abi::Socket, event: abi::event::Event) -> io::Result<()> {
	socket_map::lock()
		.get_mut(socket)?
		.async_socket
		.get_event()?
		.add_event(event)
}

#[no_mangle]
pub fn sys_event_modify(socket: abi::Socket, event: abi::event::Event) -> io::Result<()> {
	socket_map::lock()
		.get_mut(socket)?
		.async_socket
		.get_event()?
		.modify_event(event)
}

#[no_mangle]
pub fn sys_event_remove(socket: abi::Socket, target: abi::Socket) -> io::Result<()> {
	socket_map::lock()
		.get_mut(socket)?
		.async_socket
		.get_event()?
		.remove_socket(target)
}

#[no_mangle]
pub fn sys_event_wait(socket: abi::Socket, events: &mut [MaybeUninit<Event>]) -> io::Result<usize> {
	let (mut async_socket, options) = socket_map::lock().get(socket)?.split_cloned();
	let async_socket = async_socket.get_event()?;
	if options.non_blocking {
		async_socket.fill_events(events)
	} else {
		block_on(async_socket.wait_for_events(), options.timeout)?;
		async_socket.fill_events(events)
	}
}

// ---- waker ----

#[no_mangle]
pub fn sys_waker_bind(socket: abi::Socket) -> io::Result<()> {
	debug!("binding waker socket");
	socket_map::lock().bind_socket(socket, socket::AsyncWakerSocket::new(None).into())?;
	run_executor();
	Ok(())
}

#[no_mangle]
pub fn sys_waker_send_event(socket: abi::Socket, flags: EventFlags) -> io::Result<()> {
	socket_map::lock()
		.get_mut(socket)?
		.async_socket
		.get_waker()?
		.send_event(flags);
	run_executor();
	Ok(())
}

// --- tcp ---

#[no_mangle]
pub fn sys_tcp_bind(socket: abi::Socket, local: abi::SocketAddr) -> io::Result<()> {
	debug!("binding tcp socket");
	let local = abi_to_smol!(local => IpEndpoint);
	let handle = nic::lock().with(|nic| {
		if nic.iface.has_ip_addr(local.addr) {
			Ok(nic.create_tcp_handle())
		} else {
			Err(io::Error::new(
				io::ErrorKind::AddrNotAvailable,
				"specified address not available",
			))
		}
	})?;
	socket_map::lock().bind_socket(socket, AsyncTcpSocket::new(None, handle, local).into())?;
	run_executor();
	Ok(())
}

/// make a socket listen for a connections
///
/// NOTE: this does currently not queue connections if multiple arrive therefore
///       any futher connection attempts after the first will be reset
///       ToDo: make this work please
#[no_mangle]
pub fn sys_tcp_listen(socket: abi::Socket, backlog: usize) -> Result<(), io::Error> {
	unimplemented!();
	/*
	let entry = socket_map::lock().get_mut(socket).unwrap();
	if entry.async_socket.as_tcp_ref()?.is_closed() {
		entry.async_socket.into_tcp_backlog(16)?;
	} else {
		// TODO: Error ...
	}
	let backlog = entry.async_socket.as_tcp_backlog()?;
	for async_socket in backlog.iter() {
		async_socket.listen();
	}
	*/
}

#[no_mangle]
pub fn sys_tcp_accept(socket: abi::Socket) -> Result<abi::Socket, io::Error> {
	run_executor();
	unimplemented!()
	/*if info.non_blocking {
		if socket.can_recv() {
			socket.accept()?;
		} else {
			Err(IOError!(WouldBlock,"socket recv buffer empty"))
		}
	} else {
		let socket = block_on(socket.wait_for_incoming_connection(), None)??;
		socket.read(buf)
	}*/
}

#[no_mangle]
pub fn sys_tcp_connect(socket: abi::Socket, remote: abi::SocketAddr) -> io::Result<()> {
	run_executor();
	let (mut async_socket, options) = socket_map::lock().get(socket)?.split_cloned();
	let async_socket = async_socket.get_tcp()?;
	let local = local_endpoint();
	let remote = abi_to_smol!(remote => IpEndpoint);

	if options.non_blocking {
		if !async_socket.is_open() {
			let mut async_socket = async_socket.clone();
			debug!(
				"connecting {local} to {remote}",
				local = local,
				remote = remote
			);
			async_socket.connect(remote)?;
			run_executor();
			async_socket.connect(remote)
		} else {
			async_socket.connect(remote)
		}
	} else {
		debug!(
			"connecting {local} to {remote}",
			local = local,
			remote = remote
		);
		async_socket.connect(remote)?;
		debug!("wait for connection");
		block_on(async_socket.wait_for_connection(), None)?;
		async_socket.connect(remote)
	}
}

#[no_mangle]
pub fn sys_tcp_shutdown(socket: abi::Socket, mode: abi::Shutdown) -> io::Result<()> {
	run_executor();
	let (mut async_socket, options) = socket_map::lock().get(socket)?.split_cloned();
	let async_socket = async_socket.get_tcp()?;
	match mode {
		abi::Shutdown::Read => {
			async_socket.rclose();
			// nothing to do since this just inhibits read operations on the socket
			Ok(())
		}
		abi::Shutdown::Write => {
			async_socket.wclose();
			if !options.non_blocking {
				block_on(async_socket.wait_for_remaining_packets(), options.timeout)?;
			}
			Ok(())
		}
		abi::Shutdown::Both => {
			async_socket.rclose();
			async_socket.wclose();
			if !options.non_blocking {
				block_on(async_socket.wait_for_remaining_packets(), options.timeout)?;
			}
			Ok(())
		}
	}
}

#[no_mangle]
pub fn sys_tcp_read(socket: abi::Socket, buf: &mut [u8]) -> io::Result<usize> {
	run_executor();
	let (mut async_socket, options) = socket_map::lock().get(socket)?.split_cloned();
	let async_socket = async_socket.get_tcp()?;

	if options.non_blocking {
		if async_socket.can_recv() {
			async_socket.read(buf)
		} else {
			Err(IOError!(WouldBlock, "socket recv buffer empty"))
		}
	} else {
		block_on(async_socket.wait_for_readable(), None)?;
		async_socket.read(buf)
	}
}

#[no_mangle]
pub fn sys_tcp_write(socket: abi::Socket, buf: &[u8]) -> Result<usize, io::Error> {
	run_executor();
	let (mut async_socket, options) = socket_map::lock().get(socket)?.split_cloned();
	let async_socket = async_socket.get_tcp()?;

	if options.non_blocking {
		if async_socket.can_send() {
			async_socket.write(buf)
		} else {
			Err(IOError!(WouldBlock, "socket send buffer full"))
		}
	} else {
		block_on(async_socket.wait_for_writeable(), None)?;
		async_socket.write(buf)
	}
}

///////////////////////////////////////////////////////////////////
// old interface
///////////////////////////////////////////////////////////////////

#[no_mangle]
pub fn sys_tcp_stream_connect(
	ip_slice: &[u8],
	port: u16,
	timeout: Option<u64>,
) -> Result<smol::SocketHandle, ()> {
	let handle = nic::lock().with(|nic| nic.create_tcp_handle());
	let ip_str = std::str::from_utf8(ip_slice).map_err(|_| ())?;
	let ip_address = smol::IpAddress::from_str(ip_str).map_err(|_| ())?;
	let mut async_socket = AsyncTcpSocket::from(handle);
	async_socket.local = local_endpoint().into();

	let result = async_socket.connect((ip_address, port).into());
	if result.is_err() && result.unwrap_err().kind == io::ErrorKind::WouldBlock {
		// block on connect
		block_on(
			async_socket.wait_for_connection(),
			timeout.map(|ms| smol::Duration::from_millis(ms)),
		)
		.map_err(|_| {
			debug!("timeout");
			()
		})
		.map(|_| handle)?;
		// check for success
		async_socket.connect((ip_address, port).into())
	} else {
		result
	}
	.map(|_| handle)
	.map_err(|_| ())
}

#[no_mangle]
pub fn sys_tcp_stream_read(handle: smol::SocketHandle, buffer: &mut [u8]) -> Result<usize, ()> {
	let mut socket = AsyncTcpSocket::from(handle);
	block_on(socket.wait_for_readable(), None).map_err(|_| ())?;
	let n = socket.read(buffer).map_err(|_| ())?;
	run_executor();
	Ok(n)
}

#[no_mangle]
pub fn sys_tcp_stream_write(handle: smol::SocketHandle, buffer: &[u8]) -> Result<usize, ()> {
	let mut socket = AsyncTcpSocket::from(handle);
	block_on(socket.wait_for_writeable(), None).map_err(|_| ())?;
	let n = socket.write(buffer).map_err(|_| ())?;
	run_executor();
	Ok(n)
}

#[no_mangle]
pub fn sys_tcp_stream_close(handle: smol::SocketHandle) -> Result<(), ()> {
	let mut socket = AsyncTcpSocket::from(handle);
	socket.close();
	block_on(socket.wait_for_closed(), None).map_err(|_| ())?;
	nic::lock().with(|nic| nic.socket_set.release(handle));
	Ok(())
}

//ToDo: an enum, or at least constants would be better
#[no_mangle]
pub fn sys_tcp_stream_shutdown(handle: smol::SocketHandle, how: i32) -> Result<(), ()> {
	trace!("ignore shutdown");
	Ok(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_read_timeout(
	_handle: smol::SocketHandle,
	_timeout: Option<u64>,
) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_get_read_timeout(_handle: smol::SocketHandle) -> Result<Option<u64>, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_write_timeout(
	_handle: smol::SocketHandle,
	_timeout: Option<u64>,
) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_get_write_timeout(_handle: smol::SocketHandle) -> Result<Option<u64>, ()> {
	Err(())
}

#[deprecated(since = "0.1.14", note = "Please don't use this function")]
#[no_mangle]
pub fn sys_tcp_stream_duplicate(_handle: smol::SocketHandle) -> Result<smol::SocketHandle, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_peek(_handle: smol::SocketHandle, _buf: &mut [u8]) -> Result<usize, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_nonblocking(_handle: smol::SocketHandle, _mode: bool) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_tll(_handle: smol::SocketHandle, _ttl: u32) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_get_tll(_handle: smol::SocketHandle) -> Result<u32, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_peer_addr(handle: smol::SocketHandle) -> Result<(smol::IpAddress, u16), ()> {
	let endpoint = nic::lock().with(|nic| {
		nic.socket_set
			.get::<smol::TcpSocket>(handle)
			.remote_endpoint()
	});

	Ok((endpoint.addr, endpoint.port))
}

#[no_mangle]
pub fn sys_tcp_listener_accept(
	port: u16,
) -> Result<(smol::SocketHandle, smol::IpAddress, u16), ()> {
	let handle = nic::lock().with(|nic| nic.create_tcp_handle());
	let mut async_socket = AsyncTcpSocket::from(handle);
	async_socket.local = port.into();
	async_socket.listen().map_err(|_| ())?;
	run_executor();
	block_on(async_socket.wait_for_incoming_connection(), None).map_err(|_| {
		trace!("block time out");
		()
	})?;
	trace!("got an incoming connection on tcp_listener_accept");
	let remote = async_socket.accept().map_err(|_| ())?;
	trace!("remote is {:?}", remote);

	Ok((handle, remote.addr, remote.port))
}
