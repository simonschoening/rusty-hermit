// include bindings to hermit-sys if not internally used

#[cfg(not(feature = "internal"))]
mod bindings;

#[cfg(not(feature = "internal"))]
pub use bindings::*;

// networking primitives

/// Handle to internal Socket
///
/// this is currently dependent on smoltcp since a SocketHandle 
/// is not convertible to the underlying usize
///
/// Solutions to this could be:
/// 1.  amend smoltcp (e.g. by adding function to SocketHandle)
/// 2.  replicate a Handle of similar layout, like the one currently defined
///     in this crate. 
///     BUT: This is incredibly unsafe/undefined since Rust makes no guarantees
///          about the representation of structs and it may lead to strange behaviour
///          if smoltcp changes
pub type Handle = smoltcp::socket::SocketHandle;

/// Socket with enum for Information
#[derive(Debug, PartialEq, Eq)]
pub struct Socket {
    /// Handle indentifying internal socket
    pub handle: Handle,
    /// Type of the Socket (TCP/UDP/..)
    pub socket_type: SocketType,
}

/// Information about a TcpSocket
/// used to define behaviour of some 
/// sys functions like blocking on read/write
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpInfo {
    /// addr of local socket
    pub addr: SocketAddr,
    // if event is Some, io is non_blocking
//    pub event: Option<Event>
}

/// Type of a Socket with appended information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketType {
    /// tcp socket with info
    Tcp(TcpInfo),
    /// udp socket
    Udp,
}

/// Commands to be used with sys_socket
#[derive(Debug, PartialEq, Eq)]
pub enum SocketCmd<'a> {
    /// create Socket from Type
    Create(SocketType),
    /// duplicate Socket from Reference
    Dup(&'a Socket),
    /// Close and consume socket
    Close(Socket),
}

/// Commands to be used with sys_tcp
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpCmd {
    /// make socket listen for connections
    Listen,
    /// shutdown part of a connection
    Shutdown,
    /// update the internals based on tcpinfo
    Update,
}

// replicate std::net types since they are not included in core
// I don't asspciate functions since these types should be converted
// to the std types for real use
//
// ultimately conversion traits FromAbi, AsAbi and IntoAbi should be
// introduced into std to make using these idiomatic
// since this requires the interface to be rather stable I'll 
// keep manually converting until I consider the interface adequate

#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpAddr {
    V4(Ipv4Addr),
    V6(Ipv6Addr),
}

#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ipv4Addr {
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
}

impl Ipv4Addr { 
    pub const UNSPECIFIED: Self = Self {a:0,b:0,c:0,d:0}; 
}

#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ipv6Addr {
    pub a: u16,
    pub b: u16,
    pub c: u16,
    pub d: u16,
    pub e: u16,
    pub f: u16,
    pub g: u16,
    pub h: u16,
}

impl Ipv6Addr { 
    pub const UNSPECIFIED: Self = Self {a:0,b:0,c:0,d:0,e:0,f:0,g:0,h:0}; 
}

#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocketAddr {
    V4(SocketAddrV4),
    V6(SocketAddrV6),
}

#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SocketAddrV4 {
    pub ip_addr: Ipv4Addr,
    pub port: u16,
}

impl SocketAddrV4 {
    pub const UNSPECIFIED: Self = Self { 
        ip_addr: Ipv4Addr::UNSPECIFIED, 
        port: 0 
    }; 
}

#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SocketAddrV6 {
    pub ip_addr: Ipv6Addr,
    pub port: u16,
    pub flowinfo: u32,
    pub scope_id: u32,
}

impl SocketAddrV6 {
    pub const UNSPECIFIED: Self = Self { 
        ip_addr: Ipv6Addr::UNSPECIFIED, 
        port: 0, 
        flowinfo: 0, 
        scope_id: 0 
    }; 
}

#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shutdown {
    Read,
    Write,
    Both,
}
