pub type Result<T> = core::result::Result<T,Error>;

#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub msg: &'static str,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    NotFound,
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    NotConnected,
    AddrInUse,
    AddrNotAvailable,
    BrokenPipe,
    AlreadyExists,
    WouldBlock,
    InvalidInput,
    InvalidData,
    TimedOut,
    WriteZero,
    Interrupted,
    Other,
    UnexpectedEof,
    Unsupported,
}

