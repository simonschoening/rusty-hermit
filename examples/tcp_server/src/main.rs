use std::{io::Write, net::TcpListener, time::Duration};

#[allow(unused_imports)]
extern crate hermit_sys;

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:9900")?;
    println!("listening on port 9900");

	for i in 1.. {
        let (mut stream,remote) = listener.accept()?;

        println!("client no. {} connected from {}", i, remote);

        stream.write(b"Hello from rusty hermit\n")?;

        std::thread::sleep(Duration::from_secs(1));
	}

    Ok(())
}
