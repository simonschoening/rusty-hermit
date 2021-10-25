use crate::net::{Socket, SocketAddr, SocketType};
use crate::io::Result;
//use crate::event::{Interest, Event};

extern "Rust" {
// Socket
    fn sys_socket(ty: SocketType)
        -> Result<Socket>;
    fn sys_socket_dup(socket: Socket) 
        -> Result<Socket>;
    fn sys_socket_set_non_blocking(socket: Socket, non_blocking: bool) 
        -> Result<()>;
    fn sys_socket_close(socket: Socket) 
        -> Result<()>;
//    fn sys_socket_wait(socket: Socket, interest: Interest, timeout: time::Duration) 
//        -> Result<Event, io::Error>;

// TCP
    fn sys_tcp_listen(socket: Socket, backlog: usize) 
        -> Result<()>;
    fn sys_tcp_accept(socket: Socket) 
        -> Result<Socket>;
    fn sys_tcp_connect(socket: Socket, remote: SocketAddr) 
        -> Result<()>;
    fn sys_tcp_shutdown(socket: Socket) 
        -> Result<()>;
    fn sys_tcp_read(socket: Socket, buf: &mut [u8]) 
        -> Result<usize>;
    fn sys_tcp_write(socket: Socket, buf: &[u8]) 
        -> Result<usize>;
}

pub fn socket(ty: SocketType) -> Result<Socket> {
    unsafe { sys_socket(ty) }
}

pub fn socket_dup(socket: Socket) -> Result<Socket> {
    unsafe { sys_socket_dup(socket) }
}

pub fn socket_set_non_blocking(socket: Socket, non_blocking: bool) -> Result<()> {
    unsafe { sys_socket_set_non_blocking(socket, non_blocking) }
}

pub fn socket_close(socket: Socket) -> Result<()> {
    unsafe { sys_socket_close(socket) }
}

pub fn tcp_listen(socket: Socket, backlog: usize) -> Result<()> {
    unsafe { sys_tcp_listen(socket,backlog) }
}

pub fn tcp_accept(socket: Socket) -> Result<Socket> {
    unsafe { sys_tcp_accept(socket) }
}

pub fn tcp_connect(socket: Socket, remote: SocketAddr) -> Result<()> {
    unsafe { sys_tcp_connect(socket, remote) }
}

pub fn tcp_shutdown(socket: Socket, remote: SocketAddr) -> Result<()> {
    unsafe { sys_tcp_shutdown(socket) }
}

pub fn tcp_read(socket: Socket, buf: &mut [u8]) -> Result<usize> {
    unsafe { sys_tcp_read(socket, buf) }
}

pub fn tcp_write(socket: Socket, buf: &[u8]) -> Result<usize> {
    unsafe { sys_tcp_write(socket, buf) }
}
