use abi::net::event::*;
use abi::net::*;
use core::mem::MaybeUninit;
use hermit_abi as abi;
use std::io::Write;
use std::net::TcpStream;

#[allow(unused_imports)]
extern crate hermit_sys;

const NUM_SOCKETS: usize = 8;

fn main() {
	let mut stream = TcpStream::connect("10.0.5.1:5042").unwrap();
	stream.write(b"Hello from Rusty Hermit").unwrap();
	drop(stream);

	for num in 1.. {
		let event_socket = socket().unwrap();
		event_bind(event_socket).unwrap();

		println!("creating sockets...");
		let mut sockets = Vec::new();
		for i in 0..NUM_SOCKETS {
			let socket = socket().unwrap();
			//socket_set_non_blocking(socket, true).unwrap();
			tcp_bind(
				socket,
				SocketAddr::V4(SocketAddrV4 {
					ip_addr: Ipv4Addr {
						a: 10,
						b: 0,
						c: 5,
						d: 3,
					},
					port: 0,
				}),
			)
			.unwrap();
			event_add(
				event_socket,
				Event {
					socket,
					flags: EventFlags(0b1111),
					data: i as u64,
				},
			)
			.unwrap();
			sockets.push(socket);
		}
		println!("done");

		println!("connecting sockets..");
		let mut connected = vec![false; sockets.len()];
		let addr = SocketAddr::V4(SocketAddrV4 {
			ip_addr: Ipv4Addr {
				a: 10,
				b: 0,
				c: 5,
				d: 1,
			},
			port: 5042,
		});
		let connect = |i| {
			if let Err(err) = tcp_connect(sockets[i], addr) {
				if err.kind == abi::io::ErrorKind::WouldBlock {
					false
				} else {
					panic!("{:?}", err);
				}
			} else {
				true
			}
		};
		(0..sockets.len())
			.map(connect)
			.enumerate()
			.for_each(|(i, c)| connected[i] = c);
		let mut events = vec![MaybeUninit::<Event>::uninit(); sockets.len()];
		while connected.iter().any(|&c| c == false) {
			println!("waiting for events. State: {:?}", connected.as_slice());
			let num = event_wait(event_socket, &mut events).unwrap();
			for event in unsafe { events[0..num].iter().map(|event| event.assume_init_ref()) } {
				let i = event.data as usize;
				if !connected[i] {
					connected[i] = connect(i);
				}
			}
		}
		println!("done");

		for sock in sockets {
			print!("sending data..");
			let data = format!("Verbindung {}", num);
			// data that still needs to be sent
			let mut to_send: &[u8] = data.as_bytes();
			loop {
				match tcp_write(sock, to_send) {
					Err(err) => {
						if err.kind == abi::io::ErrorKind::WouldBlock {
							print!(".");
							unsafe { abi::usleep(10_000) }
							continue;
						} else {
							panic!("{:?}", err)
						}
					}
					Ok(0) => panic!("socket closed"),
					// if something was written change to_send slice
					Ok(n) => to_send = &to_send[n..],
				}
				if to_send.is_empty() {
					break;
				};
			}
			println!("done");

			print!("reading data...");
			let mut buf = [0u8; 256];
			let read = loop {
				match tcp_read(sock, &mut buf) {
					Err(err) => {
						if err.kind == abi::io::ErrorKind::WouldBlock {
							print!(".");
							unsafe { abi::usleep(10_000) }
							continue;
						} else {
							panic!("{:?}", err)
						}
					}
					Ok(0) => panic!("socket closed"),
					// return when something was received
					Ok(n) => break n,
				}
			};
			println!("done");

			match std::str::from_utf8(&buf[..read]) {
				Ok(s) => println!("Received: {}", s),
				Err(err) => println!("Error: {}", err),
			};

			print!("closing socket...");
			//tcp_shutdown(sock,Shutdown::Both).unwrap();
			event_remove(event_socket, sock).unwrap();
			socket_close(sock).unwrap();
			println!("done");
		}

		socket_close(event_socket).unwrap();

		//unsafe { abi::usleep(1_000_000); }
	}
}
