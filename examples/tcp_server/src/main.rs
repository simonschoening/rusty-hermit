use abi::net::event::*;
use abi::net::*;
use core::mem::MaybeUninit;
use hermit_abi as abi;

#[allow(unused_imports)]
extern crate hermit_sys;

fn main() {
    let listener = socket().unwrap();
    tcp_bind(listener,SocketAddr::V4(SocketAddrV4 {
        ip_addr: Ipv4Addr::UNSPECIFIED,
        port: 9900
    })).unwrap();
    tcp_listen(listener, 16).unwrap();
    println!("listening on port 9900");

	for i in 1.. {
        let stream = tcp_accept(listener).unwrap();

        println!("client no. {} connected", i);

        socket_close(stream).unwrap();
	}
}
