use hermit_abi as abi;
use abi::net::*;

#[allow(unused_imports)]
extern crate hermit_sys;

fn main() {
    let socket = socket(
        SocketCmd::Create(SocketType::Tcp(
            TcpInfo { 
                addr: SocketAddr::V4(SocketAddrV4::UNSPECIFIED)
            }
        ))
    ).unwrap();

    let addr = SocketAddrV4 { port: 45000, .. SocketAddrV4::UNSPECIFIED };
    tcp_connect(&socket, SocketAddr::V4(addr)).unwrap();

    let addr = SocketAddrV4::UNSPECIFIED;
	println!("num: {}", unsafe { abi::rand() } );
}
