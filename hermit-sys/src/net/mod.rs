pub mod device;
mod executor;
mod waker;

#[cfg(target_arch = "aarch64")]
use aarch64::regs::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::_rdtsc;
use std::convert::TryInto;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::str::FromStr;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::u16;

mod smol {
    #[cfg(feature = "dhcpv4")]
    pub use smoltcp::dhcp::Dhcpv4Client;
    #[cfg(feature = "dhcpv4")]
    pub use smoltcp::wire::{IpCidr, Ipv4Address, Ipv4Cidr};
    #[cfg(feature = "trace")]
    pub use smoltcp::phy::EthernetTracer;

    pub use smoltcp::iface::EthernetInterface;
    pub use smoltcp::phy::Device;
    pub use smoltcp::time::{Duration, Instant};
    pub use smoltcp::wire::{IpAddress,IpEndpoint};
    pub use smoltcp::socket::{
        Socket,SocketSet,SocketHandle,TcpSocket,TcpSocketBuffer,TcpState
    };
    pub use smoltcp::Error;
}

use hermit_abi::Tid;
use hermit_abi::net as abi;
use hermit_abi::io;

use futures_lite::future;

use crate::net::device::HermitNet;
use crate::net::executor::{poll_on as block_on, spawn, run_executor};
use crate::net::waker::WakerRegistration;

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
    }
}

pub(crate) enum NetworkState {
	Missing,
	InitializationFailed,
	Initialized(NetworkInterface<HermitNet>),
}

impl NetworkState {
    pub fn with<F,R>(&mut self, f: F) -> io::Result<R> 
    where
        F: FnOnce(&mut NetworkInterface<HermitNet>) -> io::Result<R>,
    {
		match self {
			NetworkState::Initialized(nic) => f(nic),
			_ => Err(IOError!(NotFound,"Network Interface not found")),
		}
    }
}

lazy_static! {
	static ref NIC: Mutex<NetworkState> = Mutex::new(NetworkState::Missing);
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

/// Default keep alive interval in milliseconds
const DEFAULT_KEEP_ALIVE_INTERVAL: u64 = 75000;

static LOCAL_ENDPOINT: AtomicU16 = AtomicU16::new(0);
static CURRENT_SOCKET: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
pub(crate) struct MapEntry {
    handle: smol::SocketHandle,
    info: abi::SocketInfo,
    ref_count: usize,
}

pub(crate) struct NetworkInterface<T: for<'a> smol::Device<'a>> {
	#[cfg(feature = "trace")]
	pub iface: smol::EthernetInterface<'static, EthernetTracer<T>>,
	#[cfg(not(feature = "trace"))]
	pub iface: smol::EthernetInterface<'static, T>,
	pub socket_set: smol::SocketSet<'static>,
    pub socket_map: HashMap<abi::Socket,MapEntry>,
    //pub event_map: HashMap<abi::Socket,Arc<Mutex<Vec<abi::event::Event>>>>,
	#[cfg(feature = "dhcpv4")]
	dhcp: Dhcpv4Client,
	#[cfg(feature = "dhcpv4")]
	prev_cidr: Ipv4Cidr,
	waker: WakerRegistration,
}

impl<T> NetworkInterface<T>
where
	T: for<'a> smol::Device<'a>,
{
	pub(crate) fn insert_socket(
        &mut self, 
        smol_socket: smol::Socket<'static>, 
        info: abi::SocketInfo
    ) -> io::Result<abi::Socket> {
		let handle = self.socket_set.add(smol_socket);
        let abi_socket = abi::Socket { 
            id: CURRENT_SOCKET.fetch_add(1usize, Ordering::Relaxed) 
        };
        self.socket_map.insert(
            abi_socket.clone(),
            MapEntry { handle, info, ref_count: 1 },
        );

		Ok(abi_socket)
	}

	pub(crate) fn with_socket<F,R>(&mut self, handle: smol::SocketHandle, f: F) -> R
    where
        F: FnOnce(&mut smol::TcpSocket) -> R
    {
        let mut socket = self.socket_set.get::<smol::TcpSocket>(handle);
        f(&mut*socket)
    }

    pub(crate) fn get(&self, socket: abi::Socket) -> io::Result<MapEntry> {
        self.socket_map.get(&socket)
            .map(|entry| entry.clone())
            .ok_or(IOError!(InvalidInput, "Unknown Socket"))
    }

    pub(crate) fn get_mut(&mut self, socket: abi::Socket) -> io::Result<&mut MapEntry> {
        self.socket_map.get_mut(&socket)
            .ok_or(IOError!(InvalidInput, "Unknown Socket"))
    }

    pub(crate) fn remove(&mut self, socket: abi::Socket) -> io::Result<()> {
        self.socket_map.remove(&socket)
            .map(|entry| self.socket_set.release(entry.handle))
            .ok_or(IOError!(InvalidInput, "Unknown Socket"))
    }

    pub(crate) fn retain(&mut self, socket: abi::Socket) -> io::Result<()> {
        self.socket_map.get_mut(&socket)
            .map(|entry| entry.ref_count += 1)
            .ok_or(IOError!(InvalidInput, "Unknown Socket"))
    }

    pub(crate) fn release(&mut self, socket: abi::Socket) -> io::Result<()> {
        let mut remove_socket = false;
        self.socket_map
            .get_mut(&socket)
            .map(|entry| {
                if entry.ref_count > 0 { 
                    entry.ref_count -= 1;
                } else { 
                    remove_socket = true;
                }
            })
            .ok_or(IOError!(InvalidInput, "Unknown Socket"))?;
                    
            if remove_socket { self.remove(socket)?; }

            Ok(())
    }

    pub(crate) fn create_tcp(&mut self, info: abi::SocketInfo) -> io::Result<abi::Socket> {
		let tcp_rx_buffer = smol::TcpSocketBuffer::new(vec![0; 65535]);
		let tcp_tx_buffer = smol::TcpSocketBuffer::new(vec![0; 65535]);
		let tcp_socket = smol::TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
        self.insert_socket( tcp_socket.into(), info)
    }

    pub(crate) fn create_tcp_with_port(&mut self, port: Option<u16>) 
        -> io::Result<abi::Socket> 
    {
        self.create_tcp(
            abi::SocketInfo {
                socket_addr: abi::SocketAddr::V4(abi::SocketAddrV4 {
                    ip_addr: abi::Ipv4Addr::UNSPECIFIED,
                    port: port.unwrap_or_else(|| local_endpoint()),
                }),
                socket_type: abi::SocketType::Tcp,
                non_blocking: false,
            }
        )
    }

	pub(crate) fn wake(&mut self) {
		self.waker.wake()
	}

	pub(crate) fn poll_common(&mut self, timestamp: smol::Instant) {
		while self
			.iface
			.poll(&mut self.socket_set, timestamp.into())
			.unwrap_or(true)
		{
            debug!("poll_common() loop");
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
							*addr = smol::IpCidr::Ipv4(cidr);
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
					.get(&smol::IpCidr::new(smol::Ipv4Address::UNSPECIFIED.into(), 0))
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

	pub(crate) fn poll(&mut self, cx: &mut Context<'_>, timestamp: smol::Instant) {
		self.waker.register(cx.waker());
		self.poll_common(timestamp);
	}

	pub(crate) fn poll_delay(&mut self, timestamp: smol::Instant) -> Option<smol::Duration> {
		self.iface.poll_delay(&self.socket_set, timestamp)
	}
}

pub(crate) struct AsyncTcpSocket {
    handle: smol::SocketHandle,
}

impl AsyncTcpSocket {
	fn with<F,R>(&self, f: F) -> R 
    where
        F: FnOnce(&mut smol::TcpSocket) -> R
    {
        NIC
            .lock()
            .unwrap()
            .with(|nic| {
                let ret = nic.with_socket(self.handle,f);
                nic.wake();
                Ok(ret)
            })
            .unwrap()
	}

	fn try_with<F,R>(&self, f: F) -> io::Result<R> 
    where
        F: FnOnce(&mut smol::TcpSocket) -> R
    {
        use std::sync::TryLockError::*;
        NIC
            .try_lock()
            .or_else(|err| match err {
                Poisoned(_) => Err(IOError!(Other,"internal mutex poisend")),
                WouldBlock => Err(IOError!(WouldBlock,"nic mutex locked"))
            })
            .and_then(|mut state| 
                state.with(|nic| {
                    let ret = nic.with_socket(self.handle,f);
                    nic.wake();
                    Ok(ret)
                })
            )
	}

    /// Not in CLOSED, TIME-WAIT or LISTEN state
	pub(crate) fn is_active(&self) -> bool {
	    self.with( |socket| socket.is_active())
    }

    /// Not in CLOSED or TIME-WAIT state
	pub(crate) fn is_open(&self) -> bool {
	    self.with( |socket| socket.is_open())
    }

    /// In LISTENING state
	pub(crate) fn is_listening(&self) -> bool {
	    self.with( |socket| socket.is_open())
    }

    /// In ESTABLISHED state
    pub(crate) fn is_connected(&self) -> bool {
        self.with(|socket| if let smol::TcpState::Established = socket.state() {
            true
        } else {
            false
        })
    }

    /// has data is receive buffer and may recv data
    pub(crate) fn can_recv(&self) -> bool {
        self.with(|socket| socket.can_recv())
    }

    /// send buffer not full and can send
    pub(crate) fn can_send(&self) -> bool {
        self.with(|socket| socket.can_send())
    }

	pub(crate) fn connect(
        &self, 
        remote: smol::IpEndpoint,
        local: smol::IpEndpoint, 
    ) -> io::Result<()> {
	    self.with( |socket| socket.connect(
            remote,
            local,
        ))
        .map_err(|_| IOError!(Other, "can't connect. internal tcp error"))
    }

	pub(crate) fn listen(&self, local_addr: smol::IpEndpoint) -> io::Result<()> {
        self.with(|socket| socket
            .listen(local_addr)
            .map_err(|err| match err {
                smol::Error::Illegal => IOError!(Other, "already open"),
                smol::Error::Unaddressable => 
                    IOError!(InvalidInput, "port unspecified"),
                _ => IOError!(Other, "unexpected internal error"),
            }))
    }

	pub(crate) fn accept(self) -> io::Result<smol::IpEndpoint> {
		self.with(|socket| {
            socket.set_keep_alive(
                Some(smol::Duration::from_millis(DEFAULT_KEEP_ALIVE_INTERVAL)));
            Ok(socket.remote_endpoint())
        })
	}

	pub(crate) fn read(self, buffer: &mut [u8]) -> io::Result<usize> {
		self.with(|socket| {
            if socket.can_recv() {
				socket.recv_slice(buffer).map_err(|_|
                    IOError!(Other, "receive failed"))
            } else {
                Err(IOError!(Other, "Can't receive"))
            }
        })
    }

	pub(crate) fn write(self, buffer: &[u8]) -> io::Result<usize> {
		self.with(|socket| 
            if socket.can_send() {
				socket.send_slice(buffer).map_err(|_| 
                    IOError!(Other, "receive failed"))
			} else {
                Err(IOError!(Other, "can't receive"))
            }
		)
	}

	pub(crate) async fn wait_for_readable(self) -> io::Result<Self> {
		future::poll_fn(|cx| {
			self.try_with(|socket| 
                if !socket.may_recv() {
				    Poll::Ready(Err(IOError!(ConnectionAborted,"socket closed")))
                } else if socket.can_recv() {
                    Poll::Ready(Ok(()))
				} else {
                    socket.register_recv_waker(cx.waker());
                    Poll::Pending
                }
			).unwrap_or(Poll::Pending)
		}).await?;
        Ok(self)
	}

	pub(crate) async fn wait_for_writeable(self) -> io::Result<Self> {
		future::poll_fn(|cx| {
			self.try_with(|socket| 
                if !socket.may_send() {
				    Poll::Ready(Err(IOError!(ConnectionAborted,"socket closed")))
                } else if socket.can_send() {
                    Poll::Ready(Ok(()))
				} else {
                    socket.register_send_waker(cx.waker());
                    Poll::Pending
                }
			).unwrap_or(Poll::Pending)
		}).await?;
        Ok(self)
	}

    pub(crate) async fn wait_for_incoming_connection(self) -> io::Result<Self> {
		future::poll_fn(|cx| {
			self.try_with(|socket|
                if !socket.is_open() {
                    Poll::Ready(Err(IOError!(ConnectionRefused,"socket closed")))
                } else if socket.is_active() {
					Poll::Ready(Ok(()))
				} else { // socket is still listening
                    socket.register_recv_waker(cx.waker());
                    Poll::Pending
				}
			).unwrap_or(Poll::Pending)
		}).await?;
        Ok(self)
    }

	pub(crate) async fn wait_for_connection(self) -> io::Result<()> {
		future::poll_fn(|cx| {
			self.with(|socket| { 
                debug!("socket in state {:?}", socket.state());
                if !socket.is_open() {
                    Poll::Ready(Err(IOError!(ConnectionRefused, "socket closed")))
                } else if let smol::TcpState::Established = socket.state() {
                    Poll::Ready(Ok(()))
                } else {
                    socket.register_send_waker(cx.waker());
                    Poll::Pending
                }
             })
        }).await
	}

	pub(crate) async fn close(self) -> io::Result<()> {
        // check whether the connection is already closed
		let _check = future::poll_fn(|cx| {
			self.try_with(|socket| match socket.state() {
				smol::TcpState::Closed => Poll::Ready(
                    Err(IOError!(NotConnected,"socket already closed"))),
				smol::TcpState::FinWait1
				| smol::TcpState::FinWait2
				| smol::TcpState::Closing
				| smol::TcpState::TimeWait => Poll::Ready(
                    Err(IOError!(NotConnected,"socket already closing"))),
				_ => {
					if socket.send_queue() > 0 {
						socket.register_send_waker(cx.waker());
						Poll::Pending
					} else {
						socket.close();
						Poll::Ready(Ok(()))
					}
				}
			}).unwrap_or(Poll::Pending)
		}) 
        .await?;

        // fully close the connection
        // ToDo: detach a separate future which fully closes the connection 
        //       and emits an event, currently this waits an eternity in
        //       TIMEWAIT
		future::poll_fn(|cx| {
			self.with(|socket| match socket.state() {
				smol::TcpState::Closed /*| smol::TcpState::TimeWait*/ => Poll::Ready(Ok(())),
				_ => {
					socket.register_send_waker(cx.waker());
					Poll::Pending
				}
			})
		})
		.await?;

		future::poll_fn(|_| { 
            if let Ok(mut state) = NIC.try_lock() {
                Poll::Ready(
                    state
                        .with(|nic| Ok(nic.socket_set.release(self.handle)))
                )
            } else {
                Poll::Pending
            }
        })
        .await
	}
}

impl From<smol::SocketHandle> for AsyncTcpSocket {
	fn from(handle: smol::SocketHandle) -> Self {
		AsyncTcpSocket { handle }
	}
}

fn local_endpoint() -> u16 {
    LOCAL_ENDPOINT.fetch_add(1, Ordering::Acquire) | 0xC000
}

#[cfg(target_arch = "x86_64")]
fn start_endpoint() -> u16 {
	(
        (unsafe { _rdtsc() as u64 }) 
        % (u16::MAX as u64) 
    )
    .try_into()
    .unwrap()
}

#[cfg(target_arch = "aarch64")]
fn start_endpoint() -> u16 {
	(
        CNTPCT_EL0.get() 
        % (u16::MAX as u64) 
    )
    .try_into()
    .unwrap()
}

pub(crate) fn network_delay(timestamp: smol::Instant) -> Option<smol::Duration> {
	unsafe { NIC.lock().unwrap().with(|nic| Ok(nic.poll_delay(timestamp))).ok().flatten() }
}

// make this part of executor run
pub(crate) async fn network_run() {
	future::poll_fn(|cx| unsafe { 
        debug!("polling network future!");
        NIC
            .try_lock()
            .and_then(|mut state| 
                state.with(|nic| 
                    Ok({
                        debug!("running future");
                        nic.socket_set.prune();
                        nic.poll(cx, smol::Instant::now());
                        Ok(Poll::Pending)
                    })
                )
                .unwrap()
            )
            .unwrap_or(Poll::Ready(()))
	}) .await;
    error!("network_run exited");
}

extern "C" fn nic_thread(_: usize) {
	loop {
		unsafe { sys_netwait(); }

        if let Ok(mut state) = NIC.try_lock() {
            state.with(|nic| {
                nic.poll_common(smol::Instant::now());
                nic.wake();
                Ok(())
            }).unwrap();
        }
	}
}

pub(crate) fn network_init() -> io::Result<()> {
	// initialize variable, which contains the next local endpoint
	LOCAL_ENDPOINT.store(start_endpoint(), Ordering::Release);

    let mut guard = NIC.lock().unwrap();
    *guard = NetworkInterface::<HermitNet>::new();

    guard
        .with(|nic|
            Ok(nic.poll_common(smol::Instant::now()))
        )
        .and_then(|_| {
            // create thread, which manages the network stack
            // use a higher priority to reduce the network latency
            let mut tid: Tid = 0;
            let ret = unsafe { sys_spawn(&mut tid, nic_thread, 0, 3, 0) };
            if ret >= 0 {
                debug!("Spawn network thread with id {}", tid);
            }

            spawn(network_run()).detach();

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
pub fn sys_socket(mut info: abi::SocketInfo) -> io::Result<abi::Socket> {
    // Currently only UNSPECIFIED Ip adresses are ok
    // I need to check what smol does with unknown ips
    match &mut info.socket_addr {
        abi::SocketAddr::V4(abi::SocketAddrV4 { 
            ip_addr: abi::Ipv4Addr::UNSPECIFIED, ref mut port
        }) => *port = if *port != 0 { *port } else { local_endpoint() },
        abi::SocketAddr::V6(abi::SocketAddrV6 {
            ip_addr: abi::Ipv6Addr::UNSPECIFIED, ref mut port, ..
        }) => *port = if *port != 0 { *port } else { local_endpoint() },
        _ => return Err(IOError!(Unsupported, "Can't specify an address on socket creation")),
    };

    match info.socket_type {
        abi::SocketType::Tcp => NIC
            .lock()
            .unwrap()
            .with(|nic| nic.create_tcp(info) )
        ,
        _ => Err(IOError!(InvalidInput, "Unsupported Socket Type")),
    }
}

/// duplicates a socket 
#[no_mangle]
pub fn sys_socket_dup(socket: abi::Socket) -> Result<abi::Socket,io::Error> {
    // ToDo: use internal reference count of SocketSet
    NIC.lock().unwrap().with( |nic| {
        nic.retain(socket)
    })?;
    Ok(socket)
}

/// close a socket
#[no_mangle]
pub fn sys_socket_close(socket: abi::Socket) -> io::Result<()> {
    let MapEntry { handle, info, .. } = NIC
        .lock()
        .unwrap()
        .with(|nic| nic.get(socket))?;
    let async_socket = AsyncTcpSocket::from(handle);
    let task = spawn(async_socket.close());
    if info.non_blocking {
        task.detach();
        Ok(())
    } else {
        block_on(task, None)?
    }
}

/// make a socket listen for a connections
///
/// NOTE: this does currently not queue connections if multiple arrive therefore 
///       any futher connection attempts after the first will be reset
///       ToDo: make this work please
#[no_mangle] 
pub fn sys_tcp_listen(socket: abi::Socket) -> Result<(), io::Error> {
    let MapEntry { handle, info, .. } = NIC
        .lock()
        .unwrap()
        .with(|nic| nic.get(socket))?;
    let async_socket = AsyncTcpSocket::from(handle);
    async_socket.listen(abi_to_smol!(info.socket_addr => IpEndpoint))
}

#[no_mangle]
pub fn sys_tcp_connect(socket: abi::Socket, remote: abi::SocketAddr) -> io::Result<()> {
    let MapEntry { handle, info, .. } = NIC
        .lock()
        .unwrap()
        .with(|nic| nic.get(socket))?;
    let local = abi_to_smol!(info.socket_addr => IpEndpoint);
    let remote = abi_to_smol!(remote => IpEndpoint);
    let async_socket = AsyncTcpSocket::from(handle);

    if info.non_blocking {
        run_executor();
        if !async_socket.is_open() {
            debug!("connecting {local} to {remote}", local=local, remote=remote);
            async_socket.connect(remote,local)?;
            Err(IOError!(WouldBlock,"connect would block"))
        } else if !async_socket.is_connected() {
            debug!("not yet connected");
            Err(IOError!(WouldBlock,"connect would block"))
        } else {
            Ok(())
        }
    } else {
        debug!("connecting {local} to {remote}", local=local, remote=remote);
        async_socket.connect(remote,local)?;
        debug!("wait for connection");
        block_on(async_socket.wait_for_connection(), None)?
    }
}

#[no_mangle]
pub fn sys_tcp_accept(socket: abi::Socket) -> Result<abi::Socket,io::Error> {
    let MapEntry { handle, info, .. } = NIC
        .lock()
        .unwrap()
        .with(|nic| nic.get(socket))?;
    let socket = AsyncTcpSocket::from(handle);
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
pub fn sys_tcp_read(socket: abi::Socket, buf: &mut [u8]) -> Result<usize,io::Error> {
    let MapEntry { handle, info, .. } = NIC
        .lock()
        .unwrap()
        .with(|nic| nic.get(socket))?;
    let socket = AsyncTcpSocket::from(handle);
    
    if info.non_blocking {
        run_executor();
        if socket.can_recv() {
            socket.read(buf)
        } else {
            Err(IOError!(WouldBlock,"socket recv buffer empty"))
        }
    } else {
        let socket = block_on(socket.wait_for_readable(), None)??;
        socket.read(buf)
    }
}

#[no_mangle] 
pub fn sys_tcp_write(socket: abi::Socket, buf: &[u8]) -> Result<usize, io::Error> {
    let MapEntry { handle, info, .. } = NIC
        .lock()
        .unwrap()
        .with(|nic| nic.get(socket))?;
    let socket = AsyncTcpSocket::from(handle);

    if info.non_blocking {
        run_executor();
        if socket.can_send() {
            socket.write(buf)
        } else {
            Err(IOError!(WouldBlock,"socket send buffer full"))
        }
    } else {
        let socket = block_on(socket.wait_for_writeable(), None)??;
        socket.write(buf)
    }   
}

///////////////////////////////////////////////////////////////////
// old interface
///////////////////////////////////////////////////////////////////

#[no_mangle]
pub fn sys_tcp_stream_connect(
    ip_slice: &[u8], 
    port: u16, 
    timeout: Option<u64>
) -> Result<smol::SocketHandle, ()> {
	let MapEntry { handle, info, .. } = NIC
        .lock()
        .unwrap()
        .with(|nic| {
            nic.create_tcp_with_port(Some(local_endpoint()))
            .and_then(|abi_socket| {
                nic.get(abi_socket)
            })
        })
    .map_err(|_| ())?;
    let ip_str = std::str::from_utf8(ip_slice).map_err(|_| ())?;
    let ip_address = smol::IpAddress::from_str(ip_str).map_err(|_| ())?;
    let async_socket = AsyncTcpSocket::from(handle);

    async_socket.connect(
        (ip_address, port).into(), 
        abi_to_smol!(info.socket_addr => IpEndpoint)
    ).map_err(|_| ())?;
	block_on(
		async_socket.wait_for_connection(),
		timeout.map(|ms| smol::Duration::from_millis(ms)),
	)
    .map_err(|_| { debug!("timeout"); () })?
    .map(|_| handle)
    .map_err(|err| { debug!("connect failed {:?}",err); () })
}

#[no_mangle]
pub fn sys_tcp_stream_read(
    handle: smol::SocketHandle, 
    buffer: &mut [u8]
) -> Result<usize, ()> {
	let socket = AsyncTcpSocket::from(handle);
	let socket = block_on(socket.wait_for_readable(), None)
        .map_err(|_| ())?
        .map_err(|_| ())?;
    socket.read(buffer).map_err(|_| ())
}

#[no_mangle]
pub fn sys_tcp_stream_write(
    handle: smol::SocketHandle, 
    buffer: &[u8]
) -> Result<usize, ()> {
	let socket = AsyncTcpSocket::from(handle);
	let socket = block_on(socket.wait_for_writeable(), None)
        .map_err(|_| ())?
        .map_err(|_| ())?;
    socket.write(buffer).map_err(|_| ())
}

#[no_mangle]
pub fn sys_tcp_stream_close(
    handle: smol::SocketHandle
) -> Result<(), ()> {
	let socket = AsyncTcpSocket::from(handle);
	block_on(socket.close(), None)
        .map_err(|_| ())?
        .map_err(|_| ())
}

//ToDo: an enum, or at least constants would be better
#[no_mangle]
pub fn sys_tcp_stream_shutdown(
    handle: smol::SocketHandle, how: i32
) -> Result<(), ()> {
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
pub fn sys_tcp_stream_set_read_timeout(
    _handle: smol::SocketHandle, 
    _timeout: Option<u64>
) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_get_read_timeout(
    _handle: smol::SocketHandle
) -> Result<Option<u64>, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_write_timeout(
    _handle: smol::SocketHandle, 
    _timeout: Option<u64>
) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_get_write_timeout(
    _handle: smol::SocketHandle
) -> Result<Option<u64>, ()> {
	Err(())
}

#[deprecated(since = "0.1.14", note = "Please don't use this function")]
#[no_mangle]
pub fn sys_tcp_stream_duplicate(
    _handle: smol::SocketHandle
) -> Result<smol::SocketHandle, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_peek(
    _handle: smol::SocketHandle, 
    _buf: &mut [u8]
) -> Result<usize, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_nonblocking(
    _handle: smol::SocketHandle, 
    _mode: bool
) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_set_tll(
    _handle: smol::SocketHandle, 
    _ttl: u32
) -> Result<(), ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_get_tll(_handle: smol::SocketHandle) -> Result<u32, ()> {
	Err(())
}

#[no_mangle]
pub fn sys_tcp_stream_peer_addr(
    handle: smol::SocketHandle
) -> Result<(smol::IpAddress, u16), ()> {
	let endpoint = NIC
        .lock()
        .unwrap()
        .with(|nic| Ok({
            let mut socket = nic.socket_set.get::<smol::TcpSocket>(handle);
	        socket.set_keep_alive(
                Some(smol::Duration::from_millis(DEFAULT_KEEP_ALIVE_INTERVAL)));
            socket.remote_endpoint()
        }))
        .unwrap();

	Ok((endpoint.addr, endpoint.port))
}

#[no_mangle]
pub fn sys_tcp_listener_accept(
    port: u16
) -> Result<(smol::SocketHandle, smol::IpAddress, u16),()> {
    let MapEntry { handle, info, .. } = NIC
        .lock()
        .unwrap()
        .with( |nic|
            nic.create_tcp_with_port(Some(port))
            .and_then(|abi_socket| {
                nic.get(abi_socket)
            })
        ).map_err(|_| ())?;
	let async_socket = AsyncTcpSocket::from(handle);
    async_socket.listen(abi_to_smol!(info.socket_addr => IpEndpoint)).map_err(|_| ())?;
    debug!("accepting ");
	let socket = block_on(async_socket.wait_for_incoming_connection(), None)
        .map_err(|_| ())?
        .map_err(|_| ())?;

    let remote = socket.accept() .map_err(|_| ())?;

	Ok((handle, remote.addr, remote.port))
}
