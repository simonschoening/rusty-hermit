use std::task;
use smoltcp::{socket::AnySocket,time::Duration};
use futures_lite::{future,Future};
use crate::net::{nic,socket, socket_map,executor};
use hermit_abi::{net,io};

pub(crate) enum Poll<T> {
    Ready(T),
    Pending(WakeOn),
}

pub(crate) enum WakeOn {
    Send,
    Recv,
    SendRecv,
}

#[derive(Debug,Clone)]
pub(crate) enum Behaviour {
    NonBlocking,
    Blocking(Option<Duration>),
}

impl From<socket_map::Options> for Behaviour {
    fn from(options: socket_map::Options) -> Self {
        if options.non_blocking {
            Behaviour::NonBlocking
        } else {
            Behaviour::Blocking(options.timeout)
        }
    }
}

#[derive(Debug,Clone)]
pub(crate) struct PollSocketRaw<F> {
    poll_fn: F,
    handle: socket::HandleWrapper,
}

#[derive(Debug,Clone)]
pub(crate) struct PollSocketsRaw<F> {
    poll_fn: F,
    handles: Vec<socket::HandleWrapper>,
}

#[derive(Debug,Clone)]
pub(crate) struct PollEventsRaw<F> {
    poll_fn: F,
    events: Vec<net::event::Event>,
}

#[derive(Debug,Clone)]
pub(crate) struct PollSocket<F> {
    raw: PollSocketRaw<F>,
    socket: net::Socket,
    behaviour: Behaviour,
}

#[derive(Debug,Clone)]
pub(crate) struct PollSockets<F> {
    raw: PollSocketsRaw<F>,
    socket: net::Socket,
    behaviour: Behaviour,
}

#[derive(Debug,Clone)]
pub(crate) struct PollEvents<F> {
    raw: PollEventsRaw<F>,
    socket: net::Socket,
    behaviour: Behaviour,
}

impl WakeOn {
    pub(crate) fn register(self, socket: net::Socket, waker: &task::Waker, socket_map: &mut socket_map::SocketMap)
        -> io::Result<()>
    {
        let entry = socket_map.get_mut(socket)?;
        match self {
            Self::Send => entry.register_send_waker(waker),
            Self::Recv => entry.register_recv_waker(waker),
            Self::SendRecv => {
                entry.register_send_waker(waker);
                entry.register_recv_waker(waker);
            },
        }
        Ok(())
    }
}

impl<F> PollSocketRaw<F> {
    pub(crate) fn new<S,T>(handle: socket::HandleWrapper, poll_fn: F) -> Self
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        Self { handle, poll_fn }
    }

    pub(crate) fn with<S,T>(self, socket: net::Socket, behaviour: Behaviour) -> PollSocket<F>
    where 
        F: FnMut(&S) -> Poll<T>,
        S: AnySocket<'static>,
    {
        PollSocket { raw: self, socket, behaviour }
    }
}

impl<F> PollSocketsRaw<F> {
    pub(crate) fn new<S,T>(handles: Vec<socket::HandleWrapper>, poll_fn: F) -> Self
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        Self { handles, poll_fn }
    }

    pub(crate) fn with<S,T>(self, socket: net::Socket, behaviour: Behaviour) -> PollSockets<F>
    where 
        F: FnMut(&S) -> Poll<T>,
        S: AnySocket<'static>,
    {
        PollSockets { raw: self, socket, behaviour }
    }
}

impl<F> PollEventsRaw<F> {
    pub(crate) fn new<T>(events: Vec<net::event::Event>, poll_fn: F) -> Self
    where 
        F: FnMut(&net::event::Event) -> Poll<io::Result<T>>,
    {
        Self { events, poll_fn }
    }

    pub(crate) fn with<T>(self, socket: net::Socket, behaviour: Behaviour) -> PollEvents<F>
    where 
        F: FnMut(&net::event::Event) -> Poll<T>,
    {
        PollEvents { raw: self, socket, behaviour }
    }
}

impl<F> PollSocket<F> {
    pub(crate) fn execute<S,T>(mut self) -> io::Result<T>
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        match self.behaviour {
            Behaviour::NonBlocking => 
                self.poll_once(),
            Behaviour::Blocking(timeout) => 
                executor::block_on(self.into_future(), timeout)?,
        }
    }

    pub(crate) fn poll_with_socket<S,T>(&mut self) -> Poll<io::Result<T>> 
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        let PollSocketRaw { handle , poll_fn } = &mut self.raw;
        nic::lock()
            .with(|nic| 
                nic.with_ref(handle,poll_fn))
    }

    pub(crate) fn poll_once<S,T>(&mut self) -> io::Result<T> 
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        match self.poll_with_socket() {
            Poll::Ready(t) => t,
            Poll::Pending(_) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                &"this operation would block",
            )),
        }
    }

    pub(crate) fn into_future<S,T>(mut self) -> impl Future<Output = io::Result<T>> 
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        future::poll_fn(move |cx| {
            match self.poll_with_socket() {
                Poll::Ready(t) => task::Poll::Ready(t),
                Poll::Pending(wake_on) => 
                    wake_on.register(self.socket,cx.waker(),&mut socket_map::lock())
                        .map(|()| 
                            task::Poll::Pending)
                        .unwrap_or_else(|err| 
                            task::Poll::Ready(Err(err)))
            }
        })
    }
}

impl<F> PollSockets<F> {
    pub(crate) fn execute<S,T>(mut self) -> io::Result<T>
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        match self.behaviour {
            Behaviour::NonBlocking => 
                self.poll_once(),
            Behaviour::Blocking(timeout) => 
                executor::block_on(self.into_future(), timeout)?,
        }
    }

    pub(crate) fn poll_with_sockets<S,T>(&mut self) -> Poll<io::Result<T>> 
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        nic::lock().with(move |nic| {
            let PollSocketsRaw { handles , poll_fn } = &mut self.raw;
            let mut wake = None;
            let opt = handles
                .iter()
                .find_map(|handle|
                    match nic.with_ref(handle,|s| poll_fn(s)) {
                        Poll::Ready(t) => Some(t),
                        Poll::Pending(wake_on) => {
                            wake = Some(wake_on);
                            None
                        },
                    }
                );
            if let Some(t) = opt {
                Poll::Ready(t)
            } else if let Some(wake_on) = wake {
                Poll::Pending(wake_on)
            } else {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::Other,
                    &"no sockets in backlog",
                )))
            }
        })
    }

    pub(crate) fn poll_once<S,T>(&mut self) -> io::Result<T> 
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        match self.poll_with_sockets() {
            Poll::Ready(t) => t,
            Poll::Pending(_) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                &"this operation would block",
            )),
        }
    }

    pub(crate) fn into_future<S,T>(mut self) -> impl Future<Output = io::Result<T>> 
    where 
        F: FnMut(&S) -> Poll<io::Result<T>>,
        S: AnySocket<'static>,
    {
        future::poll_fn(move |cx| {
            match self.poll_with_sockets() { Poll::Ready(t) => task::Poll::Ready(t), Poll::Pending(wake_on) => 
                    wake_on.register(self.socket,cx.waker(),&mut socket_map::lock())
                        .map(|()| 
                            task::Poll::Pending)
                        .unwrap_or_else(|err| 
                            task::Poll::Ready(Err(err)))
            }
        })
    }
}

impl<F> PollEvents<F> {
    pub(crate) fn execute<T>(mut self) -> io::Result<T>
    where 
        F: FnMut(&net::event::Event) -> Poll<io::Result<T>>,
    {
        match self.behaviour {
            Behaviour::NonBlocking => 
                self.poll_once(),
            Behaviour::Blocking(timeout) => 
                executor::block_on(self.into_future(), timeout)?,
        }
    }

    pub(crate) fn poll_with_entries<T>(&mut self, waker: Option<&task::Waker>) -> Poll<io::Result<T>> 
    where 
        F: FnMut(&net::event::Event) -> Poll<io::Result<T>>,
    {
        let socket = self.socket;
        let PollEventsRaw { events, poll_fn } = &mut self.raw;
        events
            .iter()
            .find_map(|event| {
                match poll_fn(event) {
                    Poll::Ready(t) => Some(t),
                    Poll::Pending(wake_on) if waker.is_some() => {
                        wake_on.register(socket,waker.unwrap(),&mut socket_map::lock());
                        None
                    },
                    _ => None,
                }
            })
            .map(|result| Poll::Ready(result))
            .unwrap_or(Poll::Pending(WakeOn::Send))
    }

    pub(crate) fn poll_once<T>(&mut self) -> io::Result<T> 
    where 
        F: FnMut(&net::event::Event) -> Poll<io::Result<T>>,
    {
        match self.poll_with_entries(None) {
            Poll::Ready(t) => t,
            Poll::Pending(_) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                &"this operation would block",
            )),
        }
    }

    pub(crate) fn into_future<T>(mut self) -> impl Future<Output = io::Result<T>> 
    where 
        F: FnMut(&net::event::Event) -> Poll<io::Result<T>>,
    {
        future::poll_fn(move |cx| {
            match self.poll_with_entries(Some(cx.waker())) {
                Poll::Ready(t) => task::Poll::Ready(t),
                Poll::Pending(wake_on) => 
                    wake_on.register(self.socket,cx.waker(),&mut socket_map::lock())
                        .map(|()| 
                            task::Poll::Pending)
                        .unwrap_or_else(|err| 
                            task::Poll::Ready(Err(err)))
            }
        })
    }
}
