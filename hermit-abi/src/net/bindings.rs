use crate::net::{Socket, SocketCmd, SocketAddr, TcpCmd, TcpInfo};
use crate::io::Result;
//use crate::event::{Interest, Event};

extern "Rust" {
    fn sys_socket<'a>(cmd: SocketCmd<'a>) 
        -> Result<Socket>;
    fn sys_tcp(socket: &mut Socket, cmd: TcpCmd) 
        -> Result<()>;
    fn sys_tcp_accept(socket: &Socket) 
        -> Result<Socket>;
    fn sys_tcp_connect(socket: &Socket, remote: SocketAddr) 
        -> Result<()>;
    fn sys_tcp_read(socket: &Socket, buf: &mut [u8]) 
        -> Result<usize>;
    fn sys_tcp_write(socket: &Socket, buf: &[u8]) 
        -> Result<usize>;
//    fn sys_tcp_wait(socket: Socket, interest: Interest, timeout: time::Duration) -> Result<Event, io::Error>;
}

pub fn socket(cmd: SocketCmd) -> Result<Socket> {
    unsafe { sys_socket(cmd) }
}

pub fn tcp(socket: &mut Socket, cmd: TcpCmd) -> Result<()> {
    unsafe { sys_tcp(socket, cmd) }
}

pub fn tcp_accept(socket: &Socket) -> Result<Socket> {
    unsafe { sys_tcp_accept(socket) }
}

pub fn tcp_connect(socket: &Socket, remote: SocketAddr) -> Result<()> {
    unsafe { sys_tcp_connect(socket, remote) }
}

pub fn tcp_read(socket: &Socket, buf: &mut [u8]) -> Result<usize> {
    unsafe { sys_tcp_read(socket, buf) }
}

pub fn tcp_write(socket: &Socket, buf: &[u8]) -> Result<usize> {
    unsafe { sys_tcp_write(socket, buf) }
}
