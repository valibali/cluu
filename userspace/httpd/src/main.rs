//! /bin/httpd -- minimal HTTP/1.0 server.
//!
//! Listens on port 8080, serves a single static HTML page.
//! One connection at a time.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::debug_print;
use libcluu::posix::socket;

const HTTP_PORT: u16 = 8080;
const BODY: &str = "<html><body>CLUU HTTP server</body></html>";

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let fd = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0);
    if fd < 0 {
        let _ = debug_print("HTTPD_FAIL: socket\n");
        return 1;
    }
    if socket::bind(fd, 0, HTTP_PORT) != 0 {
        let _ = debug_print("HTTPD_FAIL: bind\n");
        let _ = socket::close_socket(fd);
        return 1;
    }
    if socket::listen(fd, 1) != 0 {
        let _ = debug_print("HTTPD_FAIL: listen\n");
        let _ = socket::close_socket(fd);
        return 1;
    }
    let _ = debug_print("HTTPD_LISTENING\n");

    let response = format!(
        "HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        BODY.len(),
        BODY
    );
    let resp_bytes = response.as_bytes();

    loop {
        let client = socket::accept(fd);
        if client < 0 {
            let _ = libcluu::yield_cpu();
            continue;
        }
        let mut buf = [0u8; 1024];
        let _ = socket::recv(client, buf.as_mut_ptr(), buf.len(), 0);
        let _ = socket::send(client, resp_bytes.as_ptr(), resp_bytes.len(), 0);
        let _ = socket::close_socket(client);
    }
}
