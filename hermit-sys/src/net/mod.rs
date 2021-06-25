pub mod device;
mod executor;
mod waker;

#[cfg(target_arch = "aarch64")]
use aarch64::regs::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::_rdtsc;
use std::convert::TryInto;
use std::str::FromStr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Mutex;
use std::task::{Context, Poll};

use std::u16;

#[cfg(feature = "dhcpv4")]
use smoltcp::dhcp::Dhcpv4Client;
use smoltcp::phy::Device;
#[cfg(feature = "trace")]
use smoltcp::phy::EthernetTracer;
use smoltcp::socket::{SocketHandle, SocketSet, TcpSocket, TcpSocketBuffer, TcpState};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::IpAddress;
#[cfg(feature = "dhcpv4")]
use smoltcp::wire::{IpCidr, Ipv4Address, Ipv4Cidr};
use smoltcp::Error;

use futures_lite::future;

use crate::net::device::HermitNet;
use crate::net::executor::{block_on, spawn};
use crate::net::waker::WakerRegistration;

use hermit_abi::io;

// reduce boilerplate and convieniently use std::io:Error everywhere
macro_rules! IOError {
    ($kind:ident, $msg:literal) => {
        hermit_abi::io::Error {
            kind: hermit_abi::io::ErrorKind::$kind,
            msg: $msg,
        }
    }
}

pub(crate) enum NetworkState {
	Missing,
	InitializationFailed,
	Initialized(Mutex<NetworkInterface<HermitNet>>),
}

static mut NIC: NetworkState = NetworkState::Missing;

impl NetworkState {
    pub fn with<F,R>(&mut self, f: F) -> Result<R,io::Error> 
    where
        F: FnOnce(&mut NetworkInterface<HermitNet>) -> Result<R,io::Error>,
    {
		match self {
			NetworkState::Initialized(nic) => {
				let mut guard = nic.lock()
                        .map_err(|_| IOError!(Other,"Network Interface Poisoned"))?;
                let ret = f(&mut* guard);
                guard.wake();
                ret
			}
			_ => {
                Err(IOError!(NotFound,"Network Interface not found"))
			}
		}
    }

    pub fn with_tcp<F,R>(&mut self, handle: SocketHandle, f: F) -> io::Result<R>
    where
        F: FnOnce(&mut TcpSocket) -> Result<R,io::Error>,
    {
        self.with(|nic| {
            let mut socket = nic.sockets.get::<TcpSocket>(handle);
            f(&mut* socket)
        })
    }
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

pub type Handle = SocketHandle;
pub type Tid = u32;

/// Default keep alive interval in milliseconds
const DEFAULT_KEEP_ALIVE_INTERVAL: u64 = 75000;

static LOCAL_ENDPOINT: AtomicU16 = AtomicU16::new(0);

pub(crate) struct NetworkInterface<T: for<'a> Device<'a>> {
	#[cfg(feature = "trace")]
	pub iface: smoltcp::iface::EthernetInterface<'static, EthernetTracer<T>>,
	#[cfg(not(feature = "trace"))]
	pub iface: smoltcp::iface::EthernetInterface<'static, T>,
	pub sockets: SocketSet<'static>,
	#[cfg(feature = "dhcpv4")]
	dhcp: Dhcpv4Client,
	#[cfg(feature = "dhcpv4")]
	prev_cidr: Ipv4Cidr,
	waker: WakerRegistration,
}

impl<T> NetworkInterface<T>
where
	T: for<'a> Device<'a>,
{
	pub(crate) fn create_handle(&mut self) -> Result<Handle, ()> {
		let tcp_rx_buffer = TcpSocketBuffer::new(vec![0; 65535]);
		let tcp_tx_buffer = TcpSocketBuffer::new(vec![0; 65535]);
		let tcp_socket = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
		let tcp_handle = self.sockets.add(tcp_socket);

		Ok(tcp_handle)
	}

	pub(crate) fn wake(&mut self) {
		self.waker.wake()
	}

	pub(crate) fn poll_common(&mut self, timestamp: Instant) {
		while self
			.iface
			.poll(&mut self.sockets, timestamp)
			.unwrap_or(true)
		{
			// just to make progress
		}
		#[cfg(feature = "dhcpv4")]
		let config = self
			.dhcp
			.poll(&mut self.iface, &mut self.sockets, timestamp)
			.unwrap_or_else(|e| {
				debug!("DHCP: {:?}", e);
				None
			});
		#[cfg(feature = "dhcpv4")]
		config.map(|config| {
			debug!("DHCP config: {:?}", config);
			if let Some(cidr) = config.address {
				if cidr != self.prev_cidr && !cidr.address().is_unspecified() {
					self.iface.update_ip_addrs(|addrs| {
						addrs.iter_mut().next().map(|addr| {
							*addr = IpCidr::Ipv4(cidr);
						});
					});
					self.prev_cidr = cidr;
					debug!("Assigned a new IPv4 address: {}", cidr);
				}
			}

			config.router.map(|router| {
				self.iface
					.routes_mut()
					.add_default_ipv4_route(router)
					.unwrap()
			});
			self.iface.routes_mut().update(|routes_map| {
				routes_map
					.get(&IpCidr::new(Ipv4Address::UNSPECIFIED.into(), 0))
					.map(|default_route| {
						debug!("Default gateway: {}", default_route.via_router);
					});
			});

			if config.dns_servers.iter().any(|s| s.is_some()) {
				debug!("DNS servers:");
				for dns_server in config.dns_servers.iter().filter_map(|s| *s) {
					debug!("- {}", dns_server);
				}
			}
		});
	}

	pub(crate) fn poll(&mut self, cx: &mut Context<'_>, timestamp: Instant) {
		self.waker.register(cx.waker());
		self.poll_common(timestamp);
	}

	pub(crate) fn poll_delay(&mut self, timestamp: Instant) -> Option<Duration> {
		self.iface.poll_delay(&self.sockets, timestamp)
	}
}

pub(crate) struct AsyncSocket(Handle);

impl AsyncSocket {
	pub(crate) fn new() -> Self {
		match unsafe { &mut NIC } {
			NetworkState::Initialized(nic) => {
				AsyncSocket(nic.lock().unwrap().create_handle().unwrap())
			}
			_ => {
				panic!("Network isn't initialized!");
			}
		}
	}

	fn with<R>(&self, f: impl FnOnce(&mut TcpSocket) -> R) -> R 
    {
        unsafe {
            NIC.with_tcp(
                self.0, 
                |socket| Ok(f(&mut* socket))
            ).unwrap()
        }
	}

	pub(crate) async fn connect(&self, ip: &[u8], port: u16) -> Result<Handle, Error> {
        let address = IpAddress::from_str(std::str::from_utf8(ip).map_err(|_| Error::Illegal)?)
            .map_err(|_| Error::Illegal)?;

		self.with(|s: &mut TcpSocket| {
			s.connect(
				(address, port),
				LOCAL_ENDPOINT.fetch_add(1, Ordering::SeqCst),
			)
		})
		.map_err(|_| Error::Illegal)?;

		future::poll_fn(|cx| {
			self.with(|s| match s.state() {
				TcpState::Closed | TcpState::TimeWait => Poll::Ready(Err(Error::Unaddressable)),
				TcpState::Listen => Poll::Ready(Err(Error::Illegal)),
				TcpState::SynSent | TcpState::SynReceived => {
					s.register_send_waker(cx.waker());
					Poll::Pending
				}
				_ => Poll::Ready(Ok(self.0)),
			})
		})
		.await
	}

	pub(crate) async fn accept(&self, port: u16) -> Result<(IpAddress, u16), Error> {
		self.with(|s| s.listen(port).map_err(|_| Error::Illegal))?;

		future::poll_fn(|cx| {
			self.with(|s| {
				if s.is_active() {
					Poll::Ready(Ok(()))
				} else {
					match s.state() {
						TcpState::Closed
						| TcpState::Closing
						| TcpState::FinWait1
						| TcpState::FinWait2 => Poll::Ready(Err(Error::Illegal)),
						_ => {
							s.register_recv_waker(cx.waker());
							Poll::Pending
						}
					}
				}
			})
		})
		.await?;

		match unsafe { &mut NIC } {
			NetworkState::Initialized(nic) => {
				let mut guard = nic.lock().unwrap();
				let mut socket = guard.sockets.get::<TcpSocket>(self.0);
				socket.set_keep_alive(Some(Duration::from_millis(DEFAULT_KEEP_ALIVE_INTERVAL)));
				let endpoint = socket.remote_endpoint();

				Ok((endpoint.addr, endpoint.port))
			}
			_ => Err(Error::Illegal),
		}
	}

	pub(crate) async fn read(&self, buffer: &mut [u8]) -> Result<usize, Error> {
		future::poll_fn(|cx| {
			self.with(|s| match s.state() {
				TcpState::FinWait1
				| TcpState::FinWait2
				| TcpState::Closed
				| TcpState::Closing
				| TcpState::TimeWait => Poll::Ready(Err(Error::Illegal)),
				_ => {
					if s.can_recv() {
						Poll::Ready(s.recv_slice(buffer))
					} else if !s.may_recv() {
						Poll::Ready(Err(Error::Illegal))
					} else {
						s.register_recv_waker(cx.waker());
						Poll::Pending
					}
				}
			})
		})
		.await
		.map_err(|_| Error::Illegal)
	}

	pub(crate) async fn write(&self, buffer: &[u8]) -> Result<usize, Error> {
		future::poll_fn(|cx| {
			self.with(|s| match s.state() {
				TcpState::FinWait1
				| TcpState::FinWait2
				| TcpState::Closed
				| TcpState::Closing
				| TcpState::TimeWait => Poll::Ready(Err(Error::Illegal)),
				_ => {
					if !s.may_recv() {
						Poll::Ready(Ok(0))
					} else if s.can_send() {
						Poll::Ready(s.send_slice(buffer).map_err(|_| Error::Illegal))
					} else {
						s.register_send_waker(cx.waker());
						Poll::Pending
					}
				}
			})
		})
		.await
	}

	pub(crate) async fn close(&self) -> Result<(), Error> {
		future::poll_fn(|cx| {
			self.with(|s| match s.state() {
				TcpState::FinWait1
				| TcpState::FinWait2
				| TcpState::Closed
				| TcpState::Closing
				| TcpState::TimeWait => Poll::Ready(Err(Error::Illegal)),
				_ => {
					if s.send_queue() > 0 {
						s.register_send_waker(cx.waker());
						Poll::Pending
					} else {
						s.close();
						Poll::Ready(Ok(()))
					}
				}
			})
		})
		.await?;

		future::poll_fn(|cx| {
			self.with(|s| match s.state() {
				TcpState::FinWait1
				| TcpState::FinWait2
				| TcpState::Closed
				| TcpState::Closing
				| TcpState::TimeWait => Poll::Ready(Ok(())),
				_ => {
					s.register_send_waker(cx.waker());
					Poll::Pending
				}
			})
		})
		.await
	}
}

impl From<Handle> for AsyncSocket {
	fn from(handle: Handle) -> Self {
		AsyncSocket(handle)
	}
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

pub(crate) fn network_delay(timestamp: Instant) -> Option<Duration> {
	match unsafe { &mut NIC } {
		NetworkState::Initialized(nic) => nic.lock().unwrap().poll_delay(timestamp),
		_ => None,
	}
}

pub(crate) async fn network_run() {
	future::poll_fn(|cx| match unsafe { &mut NIC } {
		NetworkState::Initialized(nic) => {
			nic.lock().unwrap().poll(cx, Instant::now());
			Poll::Pending
		}
		_ => Poll::Ready(()),
	})
	.await
}

extern "C" fn nic_thread(_: usize) {
	loop {
		unsafe {
			sys_netwait();
		}

		trace!("Network thread checks the devices");

		match unsafe { &mut NIC } {
			NetworkState::Initialized(nic) => {
				nic.lock().unwrap().poll_common(Instant::now());
			}
			_ => {}
		}
	}
}

pub(crate) fn network_init() -> Result<(), ()> {
	// initialize variable, which contains the next local endpoint
	LOCAL_ENDPOINT.store(start_endpoint(), Ordering::SeqCst);

	unsafe {
		NIC = NetworkInterface::<HermitNet>::new();

		match &mut NIC {
			NetworkState::Initialized(nic) => {
				nic.lock().unwrap().poll_common(Instant::now());

				// create thread, which manages the network stack
				// use a higher priority to reduce the network latency
				let mut tid: Tid = 0;
				let ret = sys_spawn(&mut tid, nic_thread, 0, 3, 0);
				if ret >= 0 {
					debug!("Spawn network thread with id {}", tid);
				}

				spawn(network_run()).detach();

				// switch to network thread
				sys_yield();
			}
			_ => {}
		};
	}

	Ok(())
}

///////////////////////////////////////////////////////////////////
// new interface
///////////////////////////////////////////////////////////////////

use hermit_abi::net::{
    self,
    Socket, SocketType, SocketCmd, SocketAddr,
    TcpCmd, TcpInfo, 
    IpAddr, Ipv4Addr, Ipv6Addr
};

// I'm missing conversion traits from abi to std so this is 
// needed until AsAbi, IntoAbi and FromAbi are added to std.
// I'll add them once I get to editing std to be able to
// add hermit support into tokio-rs/mio which also needs these
// traits to be properly implemented
macro_rules! abi_to_std {
    ($expr:expr => SocketAddr) => {
        match $expr {
            SocketAddr::V4(addr) => {
                let ip = addr.ip_addr;
                let std_ip = std::net::Ipv4Addr::new(
                    ip.a,ip.b,ip.c,ip.d);
                let socket_addr = std::net::SocketAddrV4::new(
                    std_ip,addr.port);
                std::net::SocketAddr::from(socket_addr)
            },
            SocketAddr::V6(addr) => {
                let ip = addr.ip_addr;
                let std_ip = std::net::Ipv6Addr::new(
                    ip.a,ip.b,ip.c,ip.d,ip.e,ip.f,ip.g,ip.h);
                let socket_addr = std::net::SocketAddrV6::new(
                    std_ip,addr.port,addr.flowinfo,addr.scope_id);
                std::net::SocketAddr::from(socket_addr)
            },
        }
    };
    ($expr:expr => IpAddr) => {
        match $expr {
            IpAddr::V4(ip) => {
                let std_ip = std::net::Ipv4Addr::new(
                    ip.a,ip.b,ip.c,ip.d);
                std::net::IpAddr::from(std_ip)
            },
            IpAddr::V6(ip) => {
                let std_ip = std::net::Ipv6Addr::new(
                    ip.a,ip.b,ip.c,ip.d,ip.e,ip.f,ip.g,ip.h);
                std::net::IpAddr::from(std_ip)
            },
        }
    }
}

/// creates a new socket (TCP/UDP/...)
#[no_mangle]
pub fn sys_socket(cmd: SocketCmd<'_>)
    -> Result<Socket,io::Error> 
{
    match cmd {
        SocketCmd::Create(socket_type) => match socket_type {
            SocketType::Tcp(..) => Ok( move |nic: &mut NetworkInterface<_>| { 
                let handle = nic.create_handle().unwrap();
                Ok(Socket { handle, socket_type })
            }),
            _ => Err(IOError!(InvalidInput, "Unsupported Socket Type")),
        },
        _ => Err(IOError!(InvalidInput, "Unsupported Command")),
    }
    .and_then(|f| unsafe { NIC.with(f) } ) 
}

/// change tcp socket properties
///
/// one should close the provided socket if this function returns Err(..)
#[no_mangle] 
pub fn sys_tcp(socket: &mut Socket, cmd: TcpCmd) -> Result<(), io::Error> {
    let info = if let SocketType::Tcp(info) = socket.socket_type {
        Ok(info) } else { Err(IOError!(InvalidInput, "Not a tcp socket")) }?;

    match cmd {
        // Listen for incoming connection specified by the associated tcpinfo
        TcpCmd::Listen =>
            Ok( |nic: &mut NetworkInterface<_>| {
                let mut tcp_socket = nic.sockets.get::<TcpSocket>(socket.handle.clone());
                let local_endpoint: smoltcp::wire::IpEndpoint = 
                    abi_to_std!(info.addr => SocketAddr).into();
                tcp_socket
                    .listen(local_endpoint)
                    .map_err(|err| match err {
                        Error::Unaddressable => IOError!(InvalidInput, "port was zero"),
                        Error::Illegal => IOError!(InvalidInput, "already open"),
                        _ => IOError!(Other, "unknown error"),
                    })
            }),
        _ => Err(IOError!(InvalidInput, "Unsupported Command Type")),
    }
    .and_then(|f| unsafe { NIC.with(f) } ) 
}

#[no_mangle]
pub fn sys_tcp_connect(socket: &Socket, remote: SocketAddr) -> Result<(), io::Error> {
    let info = if let SocketType::Tcp(info) = socket.socket_type {
        Ok(info) 
    } else { Err(IOError!(InvalidInput, "Not a tcp socket")) }?;

    let local = abi_to_std!(info.addr => SocketAddr);
    let remote = abi_to_std!(remote => SocketAddr);

    unsafe {
        NIC.with_tcp(socket.handle, |socket: &mut TcpSocket| 
            socket.connect(remote, local)
            .map_err(|_| IOError!(InvalidInput, "invalid remote"))
        )?
    }

    let connect = future::poll_fn(|cx| unsafe { 
        NIC.with_tcp(
            socket.handle, 
            |socket: &mut TcpSocket| match socket.state() {
                TcpState::Closed | TcpState::TimeWait 
                    => Err(IOError!(NotFound, "address not responding")),
                TcpState::Listen 
                    => Err(IOError!(Other,"connecting socket is now listening")),
                TcpState::SynSent | TcpState::SynReceived => {
                    socket.register_send_waker(cx.waker());
                    Ok(Poll::Pending)
                },
                _ => Ok(Poll::Ready(Ok(()))),
            }
        ).unwrap_or_else(|err| Poll::Ready(Err(err)))
    });

    block_on(connect, None)
        .expect("executor error without timeout") 
}

#[no_mangle]
pub fn sys_tcp_accept(socket: &Socket) -> Result<Socket,io::Error> {
    Err(IOError!(InvalidInput, "Unimplemented"))
}

#[no_mangle] 
pub fn sys_tcp_read(socket: &Socket, buf: &mut [u8]) -> Result<usize,io::Error> {
    let socket = AsyncSocket::from(socket.handle);
	block_on(socket.read(buf), None)
        .map_err(|_| IOError!(TimedOut, "async executor timeout"))?
        .map_err(|_| IOError!(Other, "read failed"))
}

#[no_mangle] 
pub fn sys_tcp_write(socket: Socket, buf: &[u8]) -> Result<usize, io::Error> {
    let socket = AsyncSocket::from(socket.handle);
	block_on(socket.write(buf), None)
        .map_err(|_| IOError!(TimedOut, "async executor timeout"))?
        .map_err(|_| IOError!(Other, "read failed"))
}

///////////////////////////////////////////////////////////////////
// old interface
///////////////////////////////////////////////////////////////////

#[no_mangle]
pub fn sys_tcp_stream_connect(ip: &[u8], port: u16, timeout: Option<u64>) -> Result<Handle, ()> {
	let socket = AsyncSocket::new();

	block_on(
		socket.connect(ip, port),
		timeout.map(|ms| Duration::from_millis(ms)),
	)?
	.map_err(|_| ())
}

#[no_mangle]
pub fn sys_tcp_stream_read(handle: Handle, buffer: &mut [u8]) -> Result<usize, ()> {
	let socket = AsyncSocket::from(handle);
	block_on(socket.read(buffer), None)?.map_err(|_| ())
}

#[no_mangle]
pub fn sys_tcp_stream_write(handle: Handle, buffer: &[u8]) -> Result<usize, ()> {
	let socket = AsyncSocket::from(handle);
	block_on(socket.write(buffer), None)?.map_err(|_| ())
}

#[no_mangle]
pub fn sys_tcp_stream_close(handle: Handle) -> Result<(), ()> {
	let socket = AsyncSocket::from(handle);
	block_on(socket.close(), None)?.map_err(|_| ())
}

//ToDo: an enum, or at least constants would be better
#[no_mangle]
pub fn sys_tcp_stream_shutdown(handle: Handle, how: i32) -> Result<(), ()> {
	match how {
		0 /* Read */ => {
			trace!("Shutdown::Read is not implemented");
			Ok(())
		},
		1 /* Write */ => {
			sys_tcp_stream_close(handle)
		},
		2 /* Both */ => {
			sys_tcp_stream_close(handle)
		},
		_ => {
			panic!("Invalid shutdown argument {}", how);
		},
	}
}

#[no_mangle]
pub fn sys_tcp_stream_set_read_timeout(_handle: Handle, _timeout: Option<u64>) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_get_read_timeout(_handle: Handle) -> Result<Option<u64>, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_write_timeout(_handle: Handle, _timeout: Option<u64>) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_get_write_timeout(_handle: Handle) -> Result<Option<u64>, ()> {
	Err(())
}

#[deprecated(since = "0.1.14", note = "Please don't use this function")]
#[no_mangle]
pub fn sys_tcp_stream_duplicate(_handle: Handle) -> Result<Handle, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_peek(_handle: Handle, _buf: &mut [u8]) -> Result<usize, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_nonblocking(_handle: Handle, _mode: bool) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_tll(_handle: Handle, _ttl: u32) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_get_tll(_handle: Handle) -> Result<u32, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_peer_addr(handle: Handle) -> Result<(IpAddress, u16), ()> {
	let mut guard = match unsafe { &mut NIC } {
		NetworkState::Initialized(nic) => nic.lock().unwrap(),
		_ => return Err(()),
	};
	let mut socket = guard.sockets.get::<TcpSocket>(handle);
	socket.set_keep_alive(Some(Duration::from_millis(DEFAULT_KEEP_ALIVE_INTERVAL)));
	let endpoint = socket.remote_endpoint();

	Ok((endpoint.addr, endpoint.port))
}

#[no_mangle]
pub fn sys_tcp_listener_accept(port: u16) -> Result<(Handle, IpAddress, u16), ()> {
	let socket = AsyncSocket::new();
	let (addr, port) = block_on(socket.accept(port), None)?.map_err(|_| ())?;

	Ok((socket.0, addr, port))
}
