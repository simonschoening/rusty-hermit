use crate::net::{Socket, SocketAddr, SocketInfo};
use crate::io::Result;
//use crate::event::{Interest, Event};

extern "Rust" {
// Socket
    fn sys_socket(info: SocketInfo) 
        -> Result<Socket>;
    fn sys_socket_dup(socket: Socket) 
        -> Result<Socket>;
    fn sys_socket_update(socket: Socket, info: Option<SocketInfo>) 
        -> Result<SocketInfo>;
    fn sys_socket_close(socket: Socket) 
        -> Result<()>;
//    fn sys_socket_wait(socket: Socket, interest: Interest, timeout: time::Duration) 
//        -> Result<Event, io::Error>;

// TCP
    fn sys_tcp_listen(socket: Socket) 
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

pub fn socket(info: SocketInfo) -> Result<Socket> {
    unsafe { sys_socket(info) }
}

pub fn socket_dup(socket: Socket) -> Result<Socket> {
    unsafe { sys_socket_dup(socket) }
}

pub fn socket_update(socket: Socket, info: Option<SocketInfo>) -> Result<SocketInfo> {
    unsafe { sys_socket_update(socket, info) }
}

pub fn socket_close(socket: Socket) -> Result<()> {
    unsafe { sys_socket_close(socket) }
}

pub fn tcp_listen(socket: Socket) -> Result<()> {
    unsafe { sys_tcp_listen(socket) }
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
