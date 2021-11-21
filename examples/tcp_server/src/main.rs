#![cfg_attr(target_os = "hermit", feature(rustc_private))]

use std::os::hermit::abi::net;
use std::os::hermit::io::AsAbi;
use std::{io, io::Read, io::Write, net::TcpListener, time::Duration};

#[allow(unused_imports)]
extern crate hermit_sys;

fn main() -> std::io::Result<()> {
	for arg in std::env::args() {
		print!("{:?} ", arg);
	}
	println!("");

	println!("create a tcplistener");
	let listener = TcpListener::bind("0.0.0.0:9900")?;
	let socket = listener.as_abi();
	println!("socket is {:?}", socket);
	println!("listening on port 9900");

	for i in 1.. {
		let (mut stream, remote) = listener.accept()?;

		println!("client no. {} connected from {}", i, remote);

		stream.write(b"Hello from rusty hermit\n")?;
		let mut buffer = [0u8; 16];
		stream.read_exact(&mut buffer)?;
		println!("received: {:?}", buffer);

		std::thread::sleep(Duration::from_secs(1));
	}

	Ok(())
}
