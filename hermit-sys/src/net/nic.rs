use std::sync::{Mutex,MutexGuard};
use std::task::{Context,Waker};
use crate::net::waker::WakerRegistration;
use crate::net::device::HermitNet;
use smoltcp::phy::Device;
use smoltcp::iface::EthernetInterface;
use smoltcp::socket::{SocketSet,SocketHandle,TcpSocket,TcpSocketBuffer};
use concurrent_queue::ConcurrentQueue;

lazy_static! {
	static ref NIC: Mutex<NetworkState> = Mutex::new(NetworkState::Missing);
    static ref WAKER: ConcurrentQueue<Waker> = ConcurrentQueue::unbounded();
}

/// lock the global NetworkState
///
/// This will panic if the network mutex is poisoned
pub(crate)fn lock() -> MutexGuard<'static,NetworkState> {
    NIC.lock().expect("Network State poisoned")
}

pub(crate)fn register_waker(waker: &Waker) {
    WAKER.push(waker.clone());
}

fn wake_any() {
    // wake any future waiting on socket changes on which smoltcp does not report by itself
    while let Ok(waker) = WAKER.pop() {
        trace!("waking a 'WakeOn::Any' future");
        waker.wake();
    }
}

pub(crate) enum NetworkState {
	Missing,
	InitializationFailed,
	Initialized(NetworkInterface<HermitNet>),
}

impl NetworkState {
    pub(crate)fn with<F,R>(&mut self, f: F) -> R 
    where
        F: FnOnce(&mut NetworkInterface<HermitNet>) -> R,
    {
		match self {
			NetworkState::Initialized(nic) => f(nic),
			_ => panic!("Network Interface not found"),
		}
    }
}

pub(crate) struct NetworkInterface<T: for<'a> Device<'a>> {
	#[cfg(feature = "trace")]
	pub iface: EthernetInterface<'static, EthernetTracer<T>>,
	#[cfg(not(feature = "trace"))]
	pub iface: EthernetInterface<'static, T>,
	pub socket_set: SocketSet<'static>,
	#[cfg(feature = "dhcpv4")]
	pub dhcp: Dhcpv4Client,
	#[cfg(feature = "dhcpv4")]
	pub prev_cidr: Ipv4Cidr,
	pub waker: WakerRegistration,
}

impl<T> NetworkInterface<T>
where
	T: for<'a> Device<'a>,
{
	pub(crate) fn with_tcp_socket_ref<F,R>(&mut self, handle: SocketHandle, f: F) -> R
    where
        F: FnOnce(&TcpSocket) -> R
    {
        let mut socket = self.socket_set.get::<TcpSocket>(handle);
        f(&*socket)
    }

	pub(crate) fn with_tcp_socket_mut<F,R>(&mut self, handle: SocketHandle, f: F) -> R
    where
        F: FnOnce(&mut TcpSocket) -> R
    {
        let mut socket = self.socket_set.get::<TcpSocket>(handle);
        f(&mut*socket)
    }

    pub(crate) fn create_tcp_handle(&mut self) -> SocketHandle {
		let tcp_rx_buffer = TcpSocketBuffer::new(vec![0; 65535]);
		let tcp_tx_buffer = TcpSocketBuffer::new(vec![0; 65535]);
		let tcp_socket = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
        self.socket_set.add(tcp_socket)
    }

	pub(crate) fn wake(&mut self) {
        // wake the network future
        debug!("nic has {} sockets in socket_set", self.socket_set.iter().count());
		self.waker.wake();
        wake_any();
	}

	fn poll_common(&mut self, timestamp: smoltcp::time::Instant) {
		while self
			.iface
			.poll(&mut self.socket_set, timestamp.into())
			.unwrap_or(true)
		{
            // make progress
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

	pub(crate) fn poll(&mut self, cx: &mut Context<'_>, timestamp: smoltcp::time::Instant) {
		self.waker.register(cx.waker());
		self.poll_common(timestamp);
	}

	pub(crate) fn poll_delay(&mut self, timestamp: smoltcp::time::Instant) -> Option<smoltcp::time::Duration> {
		self.iface.poll_delay(&self.socket_set, timestamp)
	}
}
