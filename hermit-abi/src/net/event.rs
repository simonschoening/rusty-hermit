
// event primitives

/// Event type 
#[repr(C)]
#[derive(Debug,Clone,Copy)]
pub struct Event {
    pub event_type: EventFlags,
    pub event_socket: EventSocket,
    pub data: u64,
} 

#[repr(C)]
#[derive(Debug,Clone,Copy)]
pub struct EventFlags(u32);
pub type EventSocket = u64;

impl EventFlags {
    const Readable: u32 = 0b0001;
    const Writable: u32 = 0b0010;
    const Closed  : u32 = 0b0100;
}
