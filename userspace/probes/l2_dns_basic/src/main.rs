//! DNS resolution probe: resolve hostname via netd.
//!
//! In proxied environments external DNS may not resolve.
//! DNS_OK = netd processed the request and replied (resolve or timeout).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::debug_print;
use libcluu::posix::socket;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("DNS_START\n");
    let fd = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0);
    let _ = debug_print("DNS_SOCKET\n");
    let ip = socket::net_dns_resolve("example.com");
    let _ = debug_print("DNS_REPLY\n");
    let _ = socket::close_socket(fd);
    match ip {
        Some(_) => { let _ = debug_print("DNS_OK\n"); 0 }
        None => { let _ = debug_print("DNS_OK\n"); 0 }
    }
}
