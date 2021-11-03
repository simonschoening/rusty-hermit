pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
	pub kind: ErrorKind,
	pub msg: &'static str,
}

impl Error {
	/// create a new io error
	pub fn new(kind: ErrorKind, msg: &'static str) -> Self {
		Self { kind, msg }
	}
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
	/// creating / adding failed
	AlreadyExists,
	/// called function on invalid socket
	NotSocket,
	/// target address not found
	NotFound,
	/// called accept on non-listening socket
	NotListening,
	/// socket is already used for another purpose
	InUse,
	/// connection refused by peer
	ConnectionRefused,
	/// connection reset by peer
	ConnectionReset,
	/// connection aborted by peer
	ConnectionAborted,
	/// tried to read from or write to unconnected socket
	NotConnected,
	/// address/port requested already in use
	AddrInUse,
	/// address/port requested not available
	AddrNotAvailable,
	/// action would block
	WouldBlock,
	/// invalid input argument configuration (e.g. register event on wrong socket type)
	InvalidInput,
	/// data in input is invalid (e.g. not events specified on sys_register_event)
	InvalidData,
	/// action timed out
	TimedOut,
	/// tried to write 0 bytes
	///
	/// since a return value of zero from a write indicates an unwritable socket
	/// a zero write, for which a successful completion would return 0, is disallowed
	WriteZero,
	/// internal errors not directly addressable in user space
	Other,
	/// currently unsupported call
	Unsupported,
}
