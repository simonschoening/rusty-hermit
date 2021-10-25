use smoltcp::socket::SocketHandle;
use async_task::Task;
use std::task::{Waker,Poll};
use std::sync::{Mutex,MutexGuard};
use futures_lite::future;

use hermit_abi::io;
use hermit_abi::net::Socket;

use crate::net::executor;
use crate::net::socket::{AsyncSocket,AsyncTcpSocket};

lazy_static!{
    static ref SOCKETS: Mutex<SocketMap> = Mutex::new(SocketMap::new());
}

pub(crate) fn lock() -> MutexGuard<'static,SocketMap> {
    SOCKETS
        .lock()
        .expect("SocketMap mutex poisoned")
}

/// this struct provides conversion from the exposed abi::Socket
/// to the internal entry
#[derive(Debug)]
pub(crate) struct SocketMap {
    sockets: Vec<Option<SocketEntry>>,
}

/// the socket_entry manages options related to calling behaviour (like non_blocking), 
/// to access settings for lower levels or query things about the socket 
/// the async_socket within is used
#[derive(Debug)]
pub(crate) struct SocketEntry {
    /// the async socket to perform operations on
    pub async_socket: AsyncSocket,
    /// whether operations should be executed asynchronously
    pub non_blocking: bool,
    /// instead of detaching async tasks which access a socket through the socket_map 
    /// they get stored here to be canceled when the entry is removed
    tasks: Vec<Task<()>>,
    // since actions are asynchronous multiple futures may be in flight waiting for io on the same socket
    // 
    // therefore the inherent waker mechanism of smoltcp, which only allows a single waker is not sufficient
    recv_wakers: Vec<Waker>,
    send_wakers: Vec<Waker>,
}

impl SocketEntry {
    pub(crate) fn register_task(&mut self, task: Task<()>) {
        self.tasks.push(task);
    }

    pub(crate) fn register_send_waker(&mut self, waker: &Waker) {
        self.send_wakers.push(waker.clone());
    }

    pub(crate) fn register_recv_waker(&mut self, waker: &Waker) {
        self.recv_wakers.push(waker.clone());
    }

    pub(crate) fn wake_recv(&mut self) {
        trace!("waking recv for {:?}",self.async_socket);
        for waker in self.recv_wakers.drain(..) {
            waker.wake();
        }
    }

    pub(crate) fn wake_send(&mut self) {
        trace!("waking send for {:?}",self.async_socket);
        for waker in self.send_wakers.drain(..) {
            waker.wake();
        }
    }
}

impl SocketMap {
    fn new() -> Self {
        Self { sockets: Vec::with_capacity(256) }
    }

    fn next_free_entry(&mut self) -> Socket {
        // find a vacant slot or create a new one
        let id = self.sockets
            .iter()
            .enumerate()
            .find_map(|(id,entry)| 
                entry.is_none().then(|| id))
            .unwrap_or_else(|| {
                self.sockets.push(None);
                self.sockets.len() - 1
            });
        Socket { id }
    }

    pub(crate) fn take(&mut self, socket: Socket) -> io::Result<SocketEntry> {
        self.sockets
            .get_mut(socket.id)
            .and_then(Option::take)
            .map(|mut entry| {
                // drop/cancel tasks but keep allocation in case it will be reinserted
                entry.tasks.drain(..).for_each(drop);
                entry
            })
            .ok_or(io::Error::new(io::ErrorKind::NotFound, "unknown socket"))
    }

    pub(crate) fn get(&self, socket: Socket) -> io::Result<&SocketEntry> {
        self.sockets.get(socket.id)
            .and_then(Option::as_ref)
            .ok_or(io::Error::new(io::ErrorKind::NotFound, "unknown socket"))
    }

    pub(crate) fn get_mut(&mut self, socket: Socket) -> io::Result<&mut SocketEntry> {
        self.sockets.get_mut(socket.id)
            .and_then(Option::as_mut)
            .ok_or(io::Error::new(io::ErrorKind::NotFound, "unknown socket"))
    }

    pub(crate) fn remove(&mut self,socket: Socket) {
        if let Some(entry) = self.sockets.get_mut(socket.id) { 
            *entry = None;
        }
    }

    pub(crate) fn new_tcp_socket(&mut self, handle: SocketHandle, non_blocking: bool) -> Socket {
        let socket = self.next_free_entry();
        self.sockets[socket.id]
            .insert(SocketEntry {
                async_socket: AsyncTcpSocket::new(socket,handle).into(),
                non_blocking,
                tasks: vec![
                    executor::spawn(
                        future::poll_fn(move |cx| {
                            let mut sockets = lock();
                            let entry = sockets.get_mut(socket).unwrap();
                            entry.wake_send();
                            entry.async_socket.as_tcp_mut().unwrap()
                                .with_socket_mut(|tcp_socket|
                                    tcp_socket
                                        .register_send_waker(cx.waker()));
                            Poll::Pending
                        })),
                    executor::spawn(
                        future::poll_fn(move |cx| {
                            let mut sockets = lock();
                            let entry = sockets.get_mut(socket).unwrap();
                            entry.wake_recv();
                            entry.async_socket.as_tcp_mut().unwrap()
                                .with_socket_mut(|tcp_socket|
                                    tcp_socket
                                        .register_recv_waker(cx.waker()));
                            Poll::Pending
                        }))
                    ],
                send_wakers: Vec::with_capacity(4),
                recv_wakers: Vec::with_capacity(4),
            });
        socket
    }
}
