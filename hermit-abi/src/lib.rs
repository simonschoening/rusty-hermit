//! `hermit-abi` is small interface to call functions from the unikernel
//! [RustyHermit](https://github.com/hermitcore/libhermit-rs).

#![no_std]
extern crate libc;

extern crate core;

#[cfg(feature = "bindings")]
pub mod tcplistener;
#[cfg(feature = "bindings")]
pub mod tcpstream;

#[cfg(feature = "net")]
pub mod net;

pub mod io;

#[cfg(feature = "bindings")]
mod bindings;

#[cfg(feature = "bindings")]
pub use bindings::*;

/// A thread handle type
pub type Tid = u32;

/// Maximum number of priorities
pub const NO_PRIORITIES: usize = 31;

/// Priority of a thread
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy)]
pub struct Priority(u8);

pub fn has_net() -> bool {
    cfg![feature = "net"]
}

pub fn is_bindings() -> bool {
    cfg![bindings = "bindings"]
}

impl Priority {
	pub const fn into(self) -> u8 {
		self.0
	}

	pub const fn from(x: u8) -> Self {
		Priority(x)
	}
}

pub const HIGH_PRIO: Priority = Priority::from(3);
pub const NORMAL_PRIO: Priority = Priority::from(2);
pub const LOW_PRIO: Priority = Priority::from(1);

/// A handle, identifying a socket
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Handle(usize);

pub const NSEC_PER_SEC: u64 = 1_000_000_000;
pub const CLOCK_REALTIME: u64 = 1;
pub const CLOCK_MONOTONIC: u64 = 4;
pub const STDIN_FILENO: libc::c_int = 0;
pub const STDOUT_FILENO: libc::c_int = 1;
pub const STDERR_FILENO: libc::c_int = 2;
pub const O_RDONLY: i32 = 0o0;
pub const O_WRONLY: i32 = 0o1;
pub const O_RDWR: i32 = 0o2;
pub const O_CREAT: i32 = 0o100;
pub const O_EXCL: i32 = 0o200;
pub const O_TRUNC: i32 = 0o1000;
pub const O_APPEND: i32 = 0o2000;

/// `timespec` is used by `clock_gettime` to retrieve the
/// current time
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct timespec {
    //        ^ ToDo: make this CamelCase
	/// seconds
	pub tv_sec: i64,
	/// nanoseconds
	pub tv_nsec: i64,
}

/// Internet protocol version.
#[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum Version {
	Unspecified,
	Ipv4,
	Ipv6,
}

/// A four-octet IPv4 address.
#[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Default)]
pub struct Ipv4Address(pub [u8; 4]);

/// A sixteen-octet IPv6 address.
#[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Default)]
pub struct Ipv6Address(pub [u8; 16]);

/// An internetworking address.
#[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum IpAddress {
	/// An unspecified address.
	/// May be used as a placeholder for storage where the address is not assigned yet.
	Unspecified,
	/// An IPv4 address.
	Ipv4(Ipv4Address),
	/// An IPv6 address.
	Ipv6(Ipv6Address),
}
