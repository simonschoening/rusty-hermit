use std::task::{Poll,Waker};
use std::mem::MaybeUninit;
use hermit_abi::net::event::{Event,EventFlags};
use hermit_abi::net::Socket;
use hermit_abi::io;
use crate::net::socket_map;
use futures_lite::future;

#[derive(Debug,Clone)]
pub(crate) struct AsyncEventSocket {
    events: Vec<Event>,
    socket: Option<Socket>,
}

impl AsyncEventSocket {
    pub(crate) fn new(socket: Option<Socket>) -> Self {
        Self { events: Vec::new(), socket }
    }

    pub(crate) fn set_socket(&mut self, socket: Socket) {
        let _ = self.socket.insert(socket);
    }

    pub(crate) fn register_exclusive_recv_waker(&mut self, waker: &Waker) {
        let mut guard = socket_map::lock();
        for Event { socket, flags, .. } in self.events.iter() {
            let _ = guard
                .get_mut(*socket)
                .map(|entry| {
                    if flags.0 & (EventFlags::READABLE | EventFlags::RCLOSED) != EventFlags::NONE {
                        entry.register_recv_waker(waker);
                    }
                    if flags.0 & (EventFlags::WRITABLE | EventFlags::WCLOSED) != EventFlags::NONE {
                        entry.register_send_waker(waker);
                    }
                });
        }
    }

    pub(crate) fn get_event_flags(&self) -> EventFlags {
        self.events
            .iter()
            .any(|Event { socket, flags, .. }| 
                 socket_map::lock()
                    .get(*socket)
                    .map(|entry|
                        entry.async_socket.get_event_flags().0 & flags.0 != EventFlags::NONE)
                    .unwrap_or(false))
            .then(||
                EventFlags(EventFlags::READABLE))
            .unwrap_or( 
                EventFlags(EventFlags::NONE))
    }

    pub(crate) fn add_event(&mut self, event: Event) -> io::Result<()> {
        assert!(!self.socket.as_ref().map(|&socket| socket == event.socket).unwrap_or(false),"can't add an event socket to itself");
        self.events
            .iter()
            .find(|e| e.socket == event.socket)
            .is_none()
            .then(|| self.events.push(event))
            .ok_or(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "an event for the specified socket already exists"))
    }

    pub(crate) fn modify_event(&mut self, event: Event) -> io::Result<()> {
        self.events
            .iter_mut()
            .find(|e| e.socket == event.socket)
            .map(|e| *e = event)
            .ok_or(io::Error::new(
                io::ErrorKind::NotFound,
                "this socket is not part of the specified event socket"))
    }

    pub(crate) fn remove_socket(&mut self, socket: Socket) -> io::Result<()> {
        self.events
            .iter()
            .enumerate()
            .find_map(|(index,e)| 
                (e.socket == socket).then(|| index))
            .map(|index| self.events.remove(index))
            .map(|_| ())
            .ok_or(io::Error::new(
                io::ErrorKind::NotFound,
                "this socket is not part of the specified event socket"))
    }

    pub(crate) fn fill_events(&self, events: &mut [MaybeUninit<Event>]) -> io::Result<usize> {
        let mut iter = events.iter_mut();
        Ok(self.events
            .iter()
            .map(|event @ Event { socket, flags, .. }| 
                 socket_map::lock()
                    .get(*socket)
                    .ok()
                    .and_then(|entry| {
                        let matching_events = entry.async_socket.get_event_flags().0 & flags.0;
                        if matching_events != EventFlags::NONE {
                            iter.next()
                                .map(|e| { e.write(Event { flags: EventFlags(matching_events), ..*event }); 1 })
                        } else { None }
                    })
                    .unwrap_or(0))
            .sum())
    }

    pub(crate) async fn wait_for_events(&self) {
        future::poll_fn(|cx| {
            if let Some(socket) = self.socket {
                if self.get_event_flags().0 & EventFlags::READABLE != EventFlags::NONE {
                    Poll::Ready(())
                } else {
                    socket_map::lock()
                        .get_mut(socket)
                        .map(|entry| {
                            entry.register_recv_waker(cx.waker());
                            Poll::Pending
                        })
                        .unwrap_or(Poll::Ready(()))
                }
            } else {
                Poll::Ready(())
            }
        }).await
    }
}
