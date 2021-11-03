// event primitives

/// Event type
#[derive(Debug, Clone, Copy)]
pub struct Event {
	pub flags: EventFlags,
	pub socket: super::Socket,
	pub data: u64,
}

/// Flags indicating which event occured
#[derive(Debug, Clone, Copy)]
pub struct EventFlags(pub u32);

impl EventFlags {
	pub const NONE: u32 = 0;

	pub const READABLE: u32 = 0b0001;
	pub const WRITABLE: u32 = 0b0010;
	pub const RCLOSED: u32 = 0b0100;
	pub const WCLOSED: u32 = 0b1000;
}
