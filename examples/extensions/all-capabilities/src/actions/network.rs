use std::io::{Read, Write};
use std::net::TcpStream;

use crate::bindings::flowstate::extension::host;

pub fn fetch_example() -> Result<(), String> {
  let mut stream = TcpStream::connect("example.com:80").map_err(|error| format!("connect: {error}"))?;
  stream
    .write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\nConnection: close\r\n\r\n")
    .map_err(|error| format!("send: {error}"))?;
  let mut response = Vec::new();
  stream
    .take(64 * 1024)
    .read_to_end(&mut response)
    .map_err(|error| format!("receive: {error}"))?;
  let status_line = String::from_utf8_lossy(&response)
    .lines()
    .next()
    .unwrap_or("empty response")
    .to_owned();
  host::set_status(&format!("Network response: {status_line}"));
  Ok(())
}
