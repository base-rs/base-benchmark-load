// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The window evaluation's fixture server: a thread per connection, answering every request with a 13-byte body, ready on `listening on ADDRESS`.

use std::io::{Read, Write};
use std::net::TcpListener;

fn main() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    println!("listening on {}", listener.local_addr().unwrap());
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            let mut pending = Vec::new();
            loop {
                let read = match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                pending.extend_from_slice(&buffer[..read]);
                while let Some(end) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
                    pending.drain(..end + 4);
                    if stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nHello, world!")
                        .is_err()
                    {
                        return;
                    }
                }
            }
        });
    }
}
