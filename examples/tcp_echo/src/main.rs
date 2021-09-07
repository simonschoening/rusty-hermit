use hermit_abi as abi;
use abi::net::*;

#[allow(unused_imports)]
extern crate hermit_sys;

fn main() { 
    for num in 1.. {
        print!("creating socket...");
        let sock = socket( SocketInfo {
            socket_addr: SocketAddr::V4(SocketAddrV4::UNSPECIFIED),
            socket_type: SocketType::Tcp,
            non_blocking: true,
        }).unwrap();
        println!("done");

        print!("connecting socket..");
        let addr = SocketAddr::V4(SocketAddrV4 { 
            ip_addr: Ipv4Addr { 
                a: 10, 
                b: 0, 
                c: 5, 
                d: 1,
            },
            port: 5042,
        });
        while let Err(err) = tcp_connect(sock, addr) {
            if err.kind == abi::io::ErrorKind::WouldBlock {
                print!(".");
                //unsafe { abi::usleep(100_000) }
                continue;
            } else {
                panic!("{:?}",err);
            }
        }
        println!("done");

        print!("sending data...");
        let data = format!("Verbindung {}", num);
        // data that still needs to be sent
        let mut to_send: &[u8] = data.as_bytes();
        loop {
            match tcp_write(sock, to_send) {
                Err(err) if err.kind == abi::io::ErrorKind::WouldBlock 
                    => continue,
                Err(err) => panic!("{:?}",err),
                Ok(0) => panic!("socket closed"),
                // if something was written change to_send slice
                Ok(n) => to_send = &to_send[n..],
            }
            if to_send.is_empty() { break };
        };
        println!("done");

        print!("reading data...");
        let mut buf = [0u8; 256];
        let read = loop {
            match tcp_read(sock, &mut buf) {
                Err(err) if err.kind == abi::io::ErrorKind::WouldBlock 
                    => continue,
                Err(err) => panic!("{:?}",err),
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
        socket_close(sock).unwrap();
        println!("done");
        unsafe { 
            abi::usleep(10_000);
        }
    }
}
