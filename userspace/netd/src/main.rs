//! `netd` — CLUU network daemon.
//!
//! Single-threaded IPC server wrapping a smoltcp TCP/IP stack.  Bridges
//! virtio-net frame I/O (via the `netdev` registry service) to smoltcp's
//! `Interface`, and exposes socket operations to client processes through
//! IPC labels (`NET_SOCKET`, `NET_BIND`, …, `NET_POLL`).
//!
//! Uses `libcluu::async_runtime` per AGENTS.md §7: TX frames are sent to
//! the virtio-net driver via `IpcCallFuture` (async, non-blocking) so the
//! recv loop stays responsive while awaiting the driver's reply.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

extern crate alloc;

#[cfg(not(test))]
#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use libcluu::async_runtime::{IpcCallFuture, Runtime};
use libcluu::boot::{process_info, TOKEN_CLOCK, TOKEN_EXTRA_0, TOKEN_IPC, TOKEN_SELF};
use libcluu::ipc::{
    call, call_with_payload, call_with_reply_buf, extract_reply_id, parse_message, reply,
    reply_with_payload, NET_ACCEPT, NET_BIND, NET_CLOSE, NET_CONNECT, NET_DNS_RESOLVE, NET_GET_MAC,
    NET_LISTEN, NET_PKT_SEND, NET_POLL, NET_RECV, NET_REGISTER_RECV, NET_SEND, NET_SOCKET,
    NET_SOCK_ICMP, NET_SOCK_RAW, NET_SOCK_TCP, NET_SOCK_UDP,
};
use libcluu::registry;
use libcluu::syscall::{
    clock_frequency, clock_now, endpoint_create, ipc_recv_any_with_sender, ipc_recv_nonblocking,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Result};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::dhcpv4::{self, Event as DhcpEvent};
use smoltcp::socket::icmp::{
    Endpoint as IcmpEndpoint, PacketMetadata as IcmpPacketMetadata, Socket as IcmpSocket,
};
use smoltcp::socket::dns::{self, Socket as DnsSocket};
use smoltcp::wire::DnsQueryType;
use smoltcp::socket::raw::{PacketMetadata as RawPacketMetadata, Socket as RawSocket};
use smoltcp::socket::tcp::Socket as TcpSocket;
use smoltcp::socket::udp::{PacketMetadata as UdpPacketMetadata, Socket as UdpSocket};
use smoltcp::time::Instant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpListenEndpoint, Ipv4Address,
    Ipv4Cidr,
};

const ETH_MTU: usize = 1514;
const IPC_BUF_SIZE: usize = 4096;
const POLL_TIMEOUT_MS: u64 = 200;
const TCP_RX_BUF: usize = 4096;
const TCP_TX_BUF: usize = 4096;
const UDP_RX_BUF: usize = 2048;
const UDP_TX_BUF: usize = 2048;
const ICMP_RX_BUF: usize = 2048;
const ICMP_TX_BUF: usize = 2048;
const RAW_RX_BUF: usize = 2048;
const RAW_TX_BUF: usize = 2048;

// ── entry ──────────────────────────────────────────────────────────────────

#[cfg(not(test))]
#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            let _ = debug_print(&format!("netd: fatal {:?}\n", e));
            -1
        }
    }
}

fn run() -> Result<()> {
    debug_print("netd: started")?;

    let info = process_info();
    let listen_ep = info.tokens[TOKEN_EXTRA_0];
    let token_self = info.tokens[TOKEN_SELF];
    let clock_token = info.tokens[TOKEN_CLOCK];
    let ipc_token = info.tokens[TOKEN_IPC];

    registry::init("netd")?;
    registry::register_output("main", listen_ep)?;
    debug_print("netd: registered as netd:main")?;

    for _ in 0..100 {
        yield_cpu()?;
    }

    let netdev_ep = match registry::subscribe_output("netdev", "main") {
        Ok(ep) => ep,
        Err(_) => {
            debug_print("netd: no netdev service — running idle (no NIC)")?;
            return run_idle(listen_ep, token_self);
        }
    };
    debug_print(&format!("netd: netdev endpoint {}", netdev_ep))?;

    let pkt_recv_ep = endpoint_create(ipc_token)?;

    let mut reg_msg = Message::new(NET_REGISTER_RECV, [pkt_recv_ep, 0, 0, 0, 0, 0], 1);
    call(netdev_ep, &mut reg_msg, IpcFlags::empty())?;
    debug_print("netd: registered recv endpoint with virtio-net")?;

    let mac = get_mac(netdev_ep)?;
    debug_print(&format!(
        "netd: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    ))?;

    let hw_addr = HardwareAddress::Ethernet(EthernetAddress::from_bytes(&mac));
    let mut config = Config::new(hw_addr);
    config.random_seed = 0x434C_5555;
    let mut device = NetdDevice::new(mac);
    let now = now_instant(clock_token);
    let mut iface = Interface::new(config, &mut device, now);
    let mut sockets = SocketSet::new(Vec::new());

    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(Ipv4Address::new(127, 0, 0, 1), 8)));
    });

    // DHCP client socket — acquires an IPv4 lease from QEMU SLIRP's built-in
    // DHCP server (10.0.2.x range).  Polled each loop iteration; the recv
    // timeout in the main loop is the yield that lets other work proceed
    // during the DHCP wait (AGENTS.md §7 — no blocking the async runtime).
    let dhcp_socket = dhcpv4::Socket::new();
    let dhcp_handle = sockets.add(dhcp_socket);

    let dns_servers = [IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 3))];
    let dns_socket = DnsSocket::new(&dns_servers, vec![None]);
    let dns_handle = sockets.add(dns_socket);

    debug_print("netd: smoltcp interface initialized")?;

    inject_loopback_arp(&mut device);
    debug_print("netd: injected loopback ARP reply for 127.0.0.1")?;

    let mut runtime = Runtime::new(token_self)?;
    let reply_ep = runtime.reply_endpoint();
    let registry_ep = registry::control_endpoint();

    let mut fd_map: BTreeMap<usize, (SocketHandle, usize)> = BTreeMap::new();
    let mut next_fd: usize = 1;
    let mut next_ephemeral_port: u16 = 49152;

    let mut buf = [0u8; IPC_BUF_SIZE];
    let mut loop_count: u32 = 0;
    let mut rx_total: u32 = 0;

    let mut pending_recv: Vec<(Option<usize>, u32, usize)> = Vec::new();
    let mut pending_dns: Vec<(usize, dns::QueryHandle, u32, u32)> = Vec::new();

    loop {
        runtime.poll_ready();
        while runtime.pop_completion().is_some() {}

        drain_tx(&mut device, &mut runtime, netdev_ep);

        let now = now_instant(clock_token);
        let _ = iface.poll(now, &mut device, &mut sockets);
        drain_tx(&mut device, &mut runtime, netdev_ep);
        runtime.poll_ready();
        while runtime.pop_completion().is_some() {}

        poll_dhcp(&mut sockets, dhcp_handle, &mut iface);

        if !pending_dns.is_empty() {
            let mut still_pending = Vec::new();
            for (rt, qhandle, label, start_ms) in pending_dns.drain(..) {
                let now_ms = now_instant(clock_token).millis() as u32;
                let result = {
                    let sock = sockets.get_mut::<DnsSocket>(dns_handle);
                    sock.get_query_result(qhandle)
                };
                match result {
                    Ok(addrs) => {
                        let ip_word = if let Some(IpAddress::Ipv4(a)) = addrs.first() {
                            let o = a.octets();
                            ((o[0] as usize) << 24) | ((o[1] as usize) << 16)
                                | ((o[2] as usize) << 8)
                                | o[3] as usize
                        } else {
                            0
                        };
                        if ip_word != 0 {
                            let r = Message::new(label, [ip_word, 0, 0, 0, 0, 0], 1);
                            let _ = reply(rt, &r, IpcFlags::empty());
                        } else {
                            let r = Message::new(label, [(-5isize) as usize, 0, 0, 0, 0, 0], 1);
                            let _ = reply(rt, &r, IpcFlags::empty());
                        }
                    }
                    Err(dns::GetQueryResultError::Failed) => {
                        let r = Message::new(label, [(-5isize) as usize, 0, 0, 0, 0, 0], 1);
                        let _ = reply(rt, &r, IpcFlags::empty());
                    }
                    Err(dns::GetQueryResultError::Pending) => {
                        if now_ms.saturating_sub(start_ms) > 10_000 {
                            let r = Message::new(label, [(-5isize) as usize, 0, 0, 0, 0, 0], 1);
                            let _ = reply(rt, &r, IpcFlags::empty());
                        } else {
                            still_pending.push((rt, qhandle, label, start_ms));
                        }
                    }
                }
            }
            pending_dns = still_pending;
        }

        if !pending_recv.is_empty() {
            let mut still_pending = Vec::new();
            for (reply_token, label, fd) in pending_recv.drain(..) {
                let delivered = match fd_map.get(&fd).copied() {
                    Some((handle, sock_type)) if sock_type == NET_SOCK_ICMP => {
                        let sock = sockets.get_mut::<IcmpSocket>(handle);
                        let mut tmp = [0u8; ICMP_RX_BUF];
                        match sock.recv_slice(&mut tmp) {
                            Ok((len, addr)) => {
                                let reply_payload = tmp[..len].to_vec();
                                let remote_word = ipv4_to_word(addr);
                                if let Some(rt) = reply_token {
                                    let r = Message::new(label, [len, remote_word, 0, 0, 0, 0], 1);
                                    let _ = reply_with_payload(rt, &r, &reply_payload);
                                }
                                true
                            }
                            Err(_) => false,
                        }
                    }
                    Some((handle, sock_type)) if sock_type == NET_SOCK_TCP => {
                        let sock = sockets.get_mut::<TcpSocket>(handle);
                        let mut tmp = vec![0u8; TCP_RX_BUF];
                        match sock.recv_slice(&mut tmp) {
                            Ok(len) if len > 0 => {
                                let reply_payload = tmp[..len].to_vec();
                                let remote = sock.remote_endpoint().unwrap_or(IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::UNSPECIFIED), 0));
                                let remote_word = ipv4_to_word(remote.addr);
                                if let Some(rt) = reply_token {
                                    let r = Message::new(label, [len, remote_word, remote.port as usize, 0, 0, 0], 1);
                                    let _ = reply_with_payload(rt, &r, &reply_payload);
                                }
                                true
                            }
                            Ok(_) => {
                                let state = sock.state();
                                if state == smoltcp::socket::tcp::State::CloseWait
                                    || state == smoltcp::socket::tcp::State::Closed
                                    || !sock.may_recv()
                                {
                                    if let Some(rt) = reply_token {
                                        let r = Message::new(label, [0, 0, 0, 0, 0, 0], 1);
                                        let _ = reply(rt, &r, IpcFlags::empty());
                                    }
                                    true
                                } else {
                                    false
                                }
                            }
                            Err(_) => false,
                        }
                    }
                    Some((handle, sock_type)) if sock_type == NET_SOCK_UDP => {
                        let sock = sockets.get_mut::<UdpSocket>(handle);
                        let mut tmp = vec![0u8; 2048];
                        match sock.recv_slice(&mut tmp) {
                            Ok((len, meta)) if len > 0 => {
                                let reply_payload = tmp[..len].to_vec();
                                let remote_word = ipv4_to_word(meta.endpoint.addr);
                                if let Some(rt) = reply_token {
                                    let r = Message::new(label, [len, remote_word, meta.endpoint.port as usize, 0, 0, 0], 1);
                                    let _ = reply_with_payload(rt, &r, &reply_payload);
                                }
                                true
                            }
                            _ => false,
                        }
                    }
                    _ => true,
                };
                if !delivered {
                    still_pending.push((reply_token, label, fd));
                }
            }
            pending_recv = still_pending;
        }

        if loop_count < 30 {
            let tx_q = device.tx_queue.len();
            let rx_q = device.rx_queue.len();
            let _ = debug_print(&format!(
                "netd: trace loop={} tx_q={} rx_q={} rx_total={}",
                loop_count, tx_q, rx_q, rx_total
            ));
        }
        loop_count += 1;

        while let Ok(len) = ipc_recv_nonblocking(pkt_recv_ep, &mut buf) {
            if let Some((msg, payload)) = parse_message(&buf[..len]) {
                let frame_len = msg.words[0];
                if frame_len > 0 && frame_len <= payload.len() {
                    device.rx_queue.push_back(payload[..frame_len].to_vec());
                    rx_total += 1;
                }
            }
        }

        let tokens = [listen_ep, registry_ep, reply_ep, pkt_recv_ep];
        match ipc_recv_any_with_sender(&tokens, &mut buf, POLL_TIMEOUT_MS) {
            Ok((idx, len, _sender)) => match idx {
                3 => {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let frame_len = msg.words[0];
                        if frame_len > 0 && frame_len <= payload.len() {
                            device.rx_queue.push_back(payload[..frame_len].to_vec());
                            rx_total += 1;
                        }
                    }
                }
                2 => {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let cookie = msg.words[5];
                        let p = if payload.is_empty() {
                            Vec::new()
                        } else {
                            payload.to_vec()
                        };
                        runtime.deliver_reply(cookie, msg, p);
                    }
                }
                1 => {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let _ = registry::handle_incoming_message(&msg, payload);
                    }
                }
                0 => {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let msg = msg.clone();
                        let payload = payload.to_vec();
                        handle_socket_op(
                            &msg,
                            &payload,
                            &mut sockets,
                            &mut iface,
                            &mut device,
                            clock_token,
                            &mut fd_map,
                            &mut next_fd,
                            &mut next_ephemeral_port,
                            dns_handle,
                            netdev_ep,
                            pkt_recv_ep,
                            &mut buf,
                            &mut pending_recv,
                            &mut pending_dns,
                        );
                    }
                    let _ = yield_cpu();
                }
                _ => {}
            },
            Err(libcluu::Error::Timeout) | Err(libcluu::Error::WouldBlock) => {}
            Err(e) => {
                let _ = debug_print(&format!("netd: recv error {:?}\n", e));
            }
        }
    }
}

fn run_idle(listen_ep: usize, token_self: usize) -> Result<()> {
    let mut runtime = Runtime::new(token_self)?;
    let reply_ep = runtime.reply_endpoint();
    let registry_ep = registry::control_endpoint();
    let mut buf = [0u8; IPC_BUF_SIZE];
    loop {
        runtime.poll_ready();
        while runtime.pop_completion().is_some() {}
        let tokens = [listen_ep, registry_ep, reply_ep];
        match ipc_recv_any_with_sender(&tokens, &mut buf, POLL_TIMEOUT_MS) {
            Ok((idx, len, _)) => {
                if idx == 1 {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let _ = registry::handle_incoming_message(&msg, payload);
                    }
                } else if idx == 2 {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let cookie = msg.words[5];
                        let p = if payload.is_empty() {
                            Vec::new()
                        } else {
                            payload.to_vec()
                        };
                        runtime.deliver_reply(cookie, msg, p);
                    }
                } else if let Some((msg, _payload)) = parse_message(&buf[..len]) {
                    let reply_token = extract_reply_id(&msg);
                    if let Some(rt) = reply_token {
                        let r = Message::new(msg.tag.label, [(-5isize) as usize, 0, 0, 0, 0, 0], 1);
                        let _ = reply(rt, &r, IpcFlags::empty());
                    }
                }
            }
            Err(libcluu::Error::Timeout) | Err(libcluu::Error::WouldBlock) => {}
            Err(e) => {
                let _ = debug_print(&format!("netd: idle recv error {:?}\n", e));
            }
        }
    }
}

fn now_instant(clock_token: usize) -> Instant {
    let ticks = clock_now(clock_token).unwrap_or(0);
    let freq = clock_frequency(clock_token).unwrap_or(1);
    let millis = if freq > 0 { ticks / (freq / 1000).max(1) } else { 0 };
    Instant::from_millis(millis as i64)
}

fn ipv4_from_word(w: usize) -> Ipv4Address {
    Ipv4Address::new(
        ((w >> 24) & 0xff) as u8,
        ((w >> 16) & 0xff) as u8,
        ((w >> 8) & 0xff) as u8,
        (w & 0xff) as u8,
    )
}

fn ipv4_to_word(addr: IpAddress) -> usize {
    let IpAddress::Ipv4(a) = addr;
    let o = a.octets();
    ((o[0] as usize) << 24) | ((o[1] as usize) << 16) | ((o[2] as usize) << 8) | o[3] as usize
}

fn inject_loopback_arp(device: &mut NetdDevice) {
    let mac = device.own_mac;
    let mut arp = vec![0u8; 42];
    arp[0..6].copy_from_slice(&mac);
    arp[6..12].copy_from_slice(&mac);
    arp[12] = 0x08; arp[13] = 0x06;
    arp[14] = 0x00; arp[15] = 0x01;
    arp[16] = 0x08; arp[17] = 0x00;
    arp[18] = 6;
    arp[19] = 4;
    arp[20] = 0x00; arp[21] = 0x02;
    arp[22..28].copy_from_slice(&mac);
    arp[28] = 127; arp[29] = 0; arp[30] = 0; arp[31] = 1;
    arp[32..38].copy_from_slice(&mac);
    arp[38] = 127; arp[39] = 0; arp[40] = 0; arp[41] = 1;
    device.rx_queue.push_back(arp);
}

fn drain_rx_tx(device: &mut NetdDevice, pkt_recv_ep: usize, netdev_ep: usize, buf: &mut [u8]) {
    while let Ok(len) = ipc_recv_nonblocking(pkt_recv_ep, buf) {
        if let Some((msg, payload)) = parse_message(&buf[..len]) {
            let frame_len = msg.words[0];
            if frame_len > 0 && frame_len <= payload.len() {
                device.rx_queue.push_back(payload[..frame_len].to_vec());
            }
        }
    }
    while let Some(frame) = device.tx_queue.pop_front() {
        let frame_len = frame.len();
        let mut tx_msg = Message::new(NET_PKT_SEND, [frame_len, 0, 0, 0, 0, 0], 1);
        let mut tx_reply = Message::new(0, [0; 6], 0);
        let _ = call_with_payload(netdev_ep, &tx_msg, &frame, &mut tx_reply);
    }
}

fn get_mac(netdev_ep: usize) -> Result<[u8; 6]> {
    let req = Message::new(NET_GET_MAC, [0, 0, 0, 0, 0, 0], 1);
    let mut reply_buf = [0u8; 64];
    let (_reply_msg, payload_len) = call_with_reply_buf(netdev_ep, &req, &[], &mut reply_buf)?;
    if payload_len < 6 {
        return Err(libcluu::Error::InvalidState);
    }
    // Payload follows the Message header in the reply buffer.
    let off = core::mem::size_of::<Message>();
    Ok([
        reply_buf[off],
        reply_buf[off + 1],
        reply_buf[off + 2],
        reply_buf[off + 3],
        reply_buf[off + 4],
        reply_buf[off + 5],
    ])
}

fn drain_tx(device: &mut NetdDevice, runtime: &mut Runtime, netdev_ep: usize) {
    while let Some(frame) = device.tx_queue.pop_front() {
        let frame_len = frame.len();
        let _ = debug_print(&format!("netd: TX frame len={} to netdev_ep={}", frame_len, netdev_ep));
        runtime.spawn(async move {
            let mut msg = Message::new(NET_PKT_SEND, [frame_len, 0, 0, 0, 0, 0], 1);
            match IpcCallFuture::new_with_payload(netdev_ep, &mut msg, &frame).await {
                Ok(_) => { let _ = debug_print("netd: TX send OK"); }
                Err(e) => { let _ = debug_print(&format!("netd: TX send err {:?}", e)); }
            }
        });
    }
}

fn poll_dhcp(sockets: &mut SocketSet, dhcp_handle: SocketHandle, iface: &mut Interface) {
    let socket = sockets.get_mut::<dhcpv4::Socket>(dhcp_handle);
    while let Some(event) = socket.poll() {
        match event {
            DhcpEvent::Configured(cfg) => {
                let octets = cfg.address.address().octets();
                iface.update_ip_addrs(|addrs| {
                    addrs.clear();
                    let _ = addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(Ipv4Address::new(127, 0, 0, 1), 8)));
                    let _ = addrs.push(IpCidr::Ipv4(cfg.address));
                });
                if let Some(router) = cfg.router {
                    let _ = iface.routes_mut().add_default_ipv4_route(router);
                }
                let _ = debug_print(&format!(
                    "netd: DHCP acquired IP {}.{}.{}.{}",
                    octets[0], octets[1], octets[2], octets[3]
                ));
            }
            DhcpEvent::Deconfigured => {
                iface.update_ip_addrs(|addrs| {
                    addrs.clear();
                    let _ = addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(Ipv4Address::new(127, 0, 0, 1), 8)));
                });
                iface.routes_mut().remove_default_ipv4_route();
            }
        }
    }
}

fn handle_socket_op(
    msg: &Message,
    payload: &[u8],
    sockets: &mut SocketSet,
    iface: &mut Interface,
    device: &mut NetdDevice,
    clock_token: usize,
    fd_map: &mut BTreeMap<usize, (SocketHandle, usize)>,
    next_fd: &mut usize,
    next_ephemeral_port: &mut u16,
    dns_handle: SocketHandle,
    netdev_ep: usize,
    pkt_recv_ep: usize,
    ipc_buf: &mut [u8],
    pending_recv: &mut Vec<(Option<usize>, u32, usize)>,
    pending_dns: &mut Vec<(usize, dns::QueryHandle, u32, u32)>,
) {
    let reply_token = extract_reply_id(msg);
    let label = msg.tag.label;
    match label {
        NET_SOCKET => {
            let sock_type = msg.words[0];
            let result = create_socket(sockets, sock_type);
            let status = match result {
                Ok(handle) => {
                    let fd = *next_fd;
                    *next_fd += 1;
                    fd_map.insert(fd, (handle, sock_type));
                    fd
                }
                Err(_) => (-1isize) as usize,
            };
            if let Some(rt) = reply_token {
                let r = Message::new(label, [status, 0, 0, 0, 0, 0], 1);
                let _ = reply(rt, &r, IpcFlags::empty());
            }
        }
        NET_CLOSE => {
            let fd = msg.words[0];
            let code = if let Some((handle, _)) = fd_map.remove(&fd) {
                let _ = sockets.remove(handle);
                0
            } else {
                (-1isize) as usize
            };
            if let Some(rt) = reply_token {
                let r = Message::new(label, [code, 0, 0, 0, 0, 0], 1);
                let _ = reply(rt, &r, IpcFlags::empty());
            }
        }
        NET_BIND => {
            let fd = msg.words[0];
            let addr_word = msg.words[1];
            let port = msg.words[2] as u16;
            let status = match fd_map.get(&fd).copied() {
                Some((handle, sock_type)) => {
                    if sock_type == NET_SOCK_TCP {
                        let endpoint = IpListenEndpoint::from((
                            IpAddress::Ipv4(ipv4_from_word(addr_word)),
                            port,
                        ));
                        let sock = sockets.get_mut::<TcpSocket>(handle);
                        if sock.state() != smoltcp::socket::tcp::State::Closed {
                            (-22isize) as usize
                        } else {
                            match sock.listen(endpoint) {
                                Ok(()) => 0,
                                Err(_) => (-22isize) as usize,
                            }
                        }
                    } else if sock_type == NET_SOCK_ICMP {
                        let sock = sockets.get_mut::<IcmpSocket>(handle);
                        match sock.bind(IcmpEndpoint::Ident(port)) {
                            Ok(()) => 0,
                            Err(_) => (-22isize) as usize,
                        }
                    } else {
                        (-22isize) as usize
                    }
                }
                None => (-9isize) as usize,
            };
            if let Some(rt) = reply_token {
                let r = Message::new(label, [status, 0, 0, 0, 0, 0], 1);
                let _ = reply(rt, &r, IpcFlags::empty());
            }
        }
        NET_CONNECT => {
            let fd = msg.words[0];
            let addr_word = msg.words[1];
            let port = msg.words[2] as u16;
            let status = match fd_map.get(&fd).copied() {
                Some((handle, sock_type)) if sock_type == NET_SOCK_TCP => {
                    let remote = IpEndpoint::new(
                        IpAddress::Ipv4(ipv4_from_word(addr_word)),
                        port,
                    );
                    let local_port = *next_ephemeral_port;
                    *next_ephemeral_port = next_ephemeral_port.wrapping_add(1).max(49152);
                    let local = IpListenEndpoint {
                        addr: None,
                        port: local_port,
                    };
                    let connect_result = {
                        let sock = sockets.get_mut::<TcpSocket>(handle);
                        sock.connect(iface.context(), remote, local)
                    };
                    match connect_result {
                        Ok(()) => {
                            inject_loopback_arp(device);
                            let mut connected = false;
                            for _ in 0..200 {
                                drain_rx_tx(device, pkt_recv_ep, netdev_ep, ipc_buf);
                                let now = now_instant(clock_token);
                                let _ = iface.poll(now, device, sockets);
                                drain_rx_tx(device, pkt_recv_ep, netdev_ep, ipc_buf);
                                let state = {
                                    let sock = sockets.get_mut::<TcpSocket>(handle);
                                    sock.state()
                                };
                                use smoltcp::socket::tcp::State::*;
                                if state == Established || state == Closed {
                                    connected = state == Established;
                                    break;
                                }
                                let _ = yield_cpu();
                            }
                            if connected { 0 } else { (-22isize) as usize }
                        }
                        Err(_) => (-22isize) as usize,
                    }
                }
                _ => (-22isize) as usize,
            };
            if let Some(rt) = reply_token {
                let r = Message::new(label, [status, 0, 0, 0, 0, 0], 1);
                let _ = reply(rt, &r, IpcFlags::empty());
            }
        }
        NET_LISTEN => {
            let fd = msg.words[0];
            let status = match fd_map.get(&fd).copied() {
                Some((handle, sock_type)) if sock_type == NET_SOCK_TCP => {
                    let sock = sockets.get_mut::<TcpSocket>(handle);
                    if sock.is_listening() {
                        0
                    } else {
                        (-22isize) as usize
                    }
                }
                _ => (-22isize) as usize,
            };
            if let Some(rt) = reply_token {
                let r = Message::new(label, [status, 0, 0, 0, 0, 0], 1);
                let _ = reply(rt, &r, IpcFlags::empty());
            }
        }
        NET_ACCEPT => {
            let fd = msg.words[0];
            let (new_fd, remote_addr, remote_port) = match fd_map.get(&fd).copied() {
                Some((listen_handle, sock_type)) if sock_type == NET_SOCK_TCP => {
                    let mut got_conn = false;
                    let mut remote = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::UNSPECIFIED), 0);
                    inject_loopback_arp(device);
                    for _ in 0..200 {
                        let is_connected = {
                            let sock = sockets.get_mut::<TcpSocket>(listen_handle);
                            !sock.is_listening() && sock.is_open()
                        };
                        if is_connected {
                            let r = {
                                let sock = sockets.get_mut::<TcpSocket>(listen_handle);
                                sock.remote_endpoint()
                            };
                            if let Some(ep) = r {
                                remote = ep;
                            }
                            got_conn = true;
                            break;
                        }
                        drain_rx_tx(device, pkt_recv_ep, netdev_ep, ipc_buf);
                        let now = now_instant(clock_token);
                        let _ = iface.poll(now, device, sockets);
                        drain_rx_tx(device, pkt_recv_ep, netdev_ep, ipc_buf);
                        let _ = yield_cpu();
                    }
                    if got_conn {
                        let new_handle = {
                            let rx = smoltcp::socket::tcp::SocketBuffer::new(vec![0; TCP_RX_BUF]);
                            let tx = smoltcp::socket::tcp::SocketBuffer::new(vec![0; TCP_TX_BUF]);
                            let new_sock = TcpSocket::new(rx, tx);
                            sockets.add(new_sock)
                        };
                        let listen_endpoint = {
                            let sock = sockets.get_mut::<TcpSocket>(listen_handle);
                            sock.listen_endpoint()
                        };
                        match new_sock_listen(sockets, new_handle, listen_endpoint) {
                            Ok(()) => {
                                let accepted_fd = *next_fd;
                                *next_fd += 1;
                                fd_map.insert(accepted_fd, (listen_handle, NET_SOCK_TCP));
                                fd_map.insert(fd, (new_handle, NET_SOCK_TCP));
                                (accepted_fd, ipv4_to_word(remote.addr), remote.port as usize)
                            }
                            Err(_) => (0usize, 0usize, 0usize),
                        }
                    } else {
                        (0usize, 0usize, 0usize)
                    }
                }
                _ => ((-9isize) as usize, 0usize, 0usize),
            };
            if let Some(rt) = reply_token {
                let r = Message::new(label, [new_fd, remote_addr, remote_port, 0, 0, 0], 1);
                let _ = reply(rt, &r, IpcFlags::empty());
            }
        }
        NET_SEND => {
            let data_len = msg.words[0];
            let fd = msg.words[1];
            let dst_word = msg.words[2];
            let status = match fd_map.get(&fd).copied() {
                Some((handle, sock_type)) => {
                    if sock_type == NET_SOCK_ICMP && dst_word != 0 {
                        let dst = ipv4_from_word(dst_word);
                        let send_result = {
                            let sock = sockets.get_mut::<IcmpSocket>(handle);
                            sock.send_slice(payload, IpAddress::Ipv4(dst))
                        };
                        match send_result {
                            Ok(()) => {
                                let now = now_instant(clock_token);
                                let _ = iface.poll(now, device, sockets);
                                drain_rx_tx(device, pkt_recv_ep, netdev_ep, ipc_buf);
                                payload.len()
                            }
                            Err(_) => (-5isize) as usize,
                        }
                    } else if sock_type == NET_SOCK_TCP {
                        let data = &payload[..data_len.min(payload.len())];
                        let send_result = {
                            let sock = sockets.get_mut::<TcpSocket>(handle);
                            sock.send_slice(data)
                        };
                        match send_result {
                            Ok(n) => {
                                let now = now_instant(clock_token);
                                let _ = iface.poll(now, device, sockets);
                                drain_rx_tx(device, pkt_recv_ep, netdev_ep, ipc_buf);
                                n
                            }
                            Err(_) => (-11isize) as usize,
                        }
                    } else if sock_type == NET_SOCK_UDP && dst_word != 0 {
                        let dst = ipv4_from_word(dst_word);
                        let dst_port = msg.words[3] as u16;
                        let data = &payload[..data_len.min(payload.len())];
                        let udp_endpoint = IpEndpoint::new(IpAddress::Ipv4(dst), dst_port);
                        let send_result = {
                            let sock = sockets.get_mut::<UdpSocket>(handle);
                            sock.send_slice(data, udp_endpoint)
                        };
                        match send_result {
                            Ok(()) => {
                                let now = now_instant(clock_token);
                                let _ = iface.poll(now, device, sockets);
                                drain_rx_tx(device, pkt_recv_ep, netdev_ep, ipc_buf);
                                data_len
                            }
                            Err(_) => (-5isize) as usize,
                        }
                    } else {
                        (-22isize) as usize
                    }
                }
                None => (-9isize) as usize,
            };
            if let Some(rt) = reply_token {
                let r = Message::new(label, [status, 0, 0, 0, 0, 0], 1);
                let _ = reply(rt, &r, IpcFlags::empty());
            }
        }
        NET_RECV => {
            let fd = msg.words[0];
            let max_len = msg.words[1];
            let is_icmp = msg.words.len() > 2 && msg.words[2] == 0xFFFF_FFFF;
            if is_icmp {
                handle_icmp_recv(reply_token, label, fd, pending_recv);
            } else {
                let (n, remote_addr, remote_port) = match fd_map.get(&fd).copied() {
                    Some((handle, sock_type)) if sock_type == NET_SOCK_TCP => {
                        let sock = sockets.get_mut::<TcpSocket>(handle);
                        let mut tmp = vec![0u8; max_len.min(TCP_RX_BUF)];
                        match sock.recv_slice(&mut tmp) {
                            Ok(len) => {
                                if len > 0 {
                                    let reply_payload = tmp[..len].to_vec();
                                    let remote = sock.remote_endpoint().unwrap_or(IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::UNSPECIFIED), 0));
                                    let remote_word = ipv4_to_word(remote.addr);
                                    if let Some(rt) = reply_token {
                                        let r = Message::new(label, [len, remote_word, remote.port as usize, 0, 0, 0], 1);
                                        let _ = reply_with_payload(rt, &r, &reply_payload);
                                    }
                                    return;
                                } else {
                                    let sock_state = sock.state();
                                    if sock_state == smoltcp::socket::tcp::State::CloseWait
                                        || sock_state == smoltcp::socket::tcp::State::Closed
                                    {
                                        (0usize, 0usize, 0usize)
                                    } else {
                                        pending_recv.push((reply_token, label, fd));
                                        return;
                                    }
                                }
                            }
                            Err(_) => ((-11isize) as usize, 0usize, 0usize),
                        }
                    }
                    Some((handle, sock_type)) if sock_type == NET_SOCK_UDP => {
                        let sock = sockets.get_mut::<UdpSocket>(handle);
                        let mut tmp = vec![0u8; max_len.min(2048)];
                        match sock.recv_slice(&mut tmp) {
                            Ok((len, meta)) => {
                                if len > 0 {
                                    let reply_payload = tmp[..len].to_vec();
                                    let remote_word = ipv4_to_word(meta.endpoint.addr);
                                    if let Some(rt) = reply_token {
                                        let r = Message::new(label, [len, remote_word, meta.endpoint.port as usize, 0, 0, 0], 1);
                                        let _ = reply_with_payload(rt, &r, &reply_payload);
                                    }
                                    return;
                                }
                                pending_recv.push((reply_token, label, fd));
                                return;
                            }
                            Err(_) => ((-11isize) as usize, 0usize, 0usize),
                        }
                    }
                    _ => ((-9isize) as usize, 0usize, 0usize),
                };
                if let Some(rt) = reply_token {
                    let r = Message::new(label, [n, remote_addr, remote_port, 0, 0, 0], 1);
                    let _ = reply(rt, &r, IpcFlags::empty());
                }
            }
        }
        NET_POLL => {
            let fd = msg.words[0];
            let readiness = match fd_map.get(&fd).copied() {
                Some((handle, sock_type)) => {
                    if sock_type == NET_SOCK_TCP {
                        let sock = sockets.get_mut::<TcpSocket>(handle);
                        let mut flags = 0u32;
                        if sock.can_recv() || (!sock.is_listening() && sock.is_open()) {
                            flags |= 1;
                        }
                        if sock.can_send() {
                            flags |= 2;
                        }
                        flags as usize
                    } else if sock_type == NET_SOCK_ICMP {
                        let sock = sockets.get_mut::<IcmpSocket>(handle);
                        if sock.recv_queue() > 0 {
                            1
                        } else {
                            0
                        }
                    } else if sock_type == NET_SOCK_UDP {
                        let sock = sockets.get_mut::<UdpSocket>(handle);
                        let mut flags = 0u32;
                        if sock.can_recv() {
                            flags |= 1;
                        }
                        if sock.can_send() {
                            flags |= 2;
                        }
                        flags as usize
                    } else {
                        0
                    }
                }
                None => (-9isize) as usize,
            };
            if let Some(rt) = reply_token {
                let r = Message::new(label, [readiness, 0, 0, 0, 0, 0], 1);
                let _ = reply(rt, &r, IpcFlags::empty());
            }
        }
        NET_DNS_RESOLVE => {
            let hostname = core::str::from_utf8(payload).unwrap_or("");
            if hostname.is_empty() {
                if let Some(rt) = reply_token {
                    let r = Message::new(label, [(-22isize) as usize, 0, 0, 0, 0, 0], 1);
                    let _ = reply(rt, &r, IpcFlags::empty());
                }
            } else {
                let query_result = {
                    let sock = sockets.get_mut::<DnsSocket>(dns_handle);
                    sock.start_query(iface.context(), hostname, DnsQueryType::A)
                };
                match query_result {
                    Ok(qhandle) => {
                        let now_ms = now_instant(clock_token).millis() as u32;
                        if let Some(rt) = reply_token {
                            pending_dns.push((rt, qhandle, label, now_ms));
                        }
                    }
                    Err(_) => {
                        if let Some(rt) = reply_token {
                            let r = Message::new(label, [(-22isize) as usize, 0, 0, 0, 0, 0], 1);
                            let _ = reply(rt, &r, IpcFlags::empty());
                        }
                    }
                }
            }
        }
        _ => {
            if let Some(rt) = reply_token {
                let r = Message::new(0, [(-22isize) as usize, 0, 0, 0, 0, 0], 1);
                let _ = reply(rt, &r, IpcFlags::empty());
            }
        }
    }
}

fn new_sock_listen(
    sockets: &mut SocketSet,
    handle: SocketHandle,
    endpoint: smoltcp::wire::IpListenEndpoint,
) -> core::result::Result<(), ()> {
    let sock = sockets.get_mut::<TcpSocket>(handle);
    sock.listen(endpoint).map_err(|_| ())
}

fn handle_icmp_recv(
    reply_token: Option<usize>,
    label: u32,
    fd: usize,
    pending_recv: &mut Vec<(Option<usize>, u32, usize)>,
) {
    pending_recv.push((reply_token, label, fd));
}

fn create_socket(sockets: &mut SocketSet, sock_type: usize) -> core::result::Result<SocketHandle, ()> {
    match sock_type {
        NET_SOCK_TCP => {
            let rx = smoltcp::socket::tcp::SocketBuffer::new(vec![0; TCP_RX_BUF]);
            let tx = smoltcp::socket::tcp::SocketBuffer::new(vec![0; TCP_TX_BUF]);
            let sock = TcpSocket::new(rx, tx);
            Ok(sockets.add(sock))
        }
        NET_SOCK_UDP => {
            let rx = smoltcp::socket::udp::PacketBuffer::new(
                vec![UdpPacketMetadata::EMPTY; 4],
                vec![0; UDP_RX_BUF],
            );
            let tx = smoltcp::socket::udp::PacketBuffer::new(
                vec![UdpPacketMetadata::EMPTY; 4],
                vec![0; UDP_TX_BUF],
            );
            let sock = UdpSocket::new(rx, tx);
            Ok(sockets.add(sock))
        }
        NET_SOCK_ICMP => {
            let rx = smoltcp::socket::icmp::PacketBuffer::new(
                vec![IcmpPacketMetadata::EMPTY; 4],
                vec![0; ICMP_RX_BUF],
            );
            let tx = smoltcp::socket::icmp::PacketBuffer::new(
                vec![IcmpPacketMetadata::EMPTY; 4],
                vec![0; ICMP_TX_BUF],
            );
            let sock = IcmpSocket::new(rx, tx);
            Ok(sockets.add(sock))
        }
        NET_SOCK_RAW => {
            let rx = smoltcp::socket::raw::PacketBuffer::new(
                vec![RawPacketMetadata::EMPTY; 4],
                vec![0; RAW_RX_BUF],
            );
            let tx = smoltcp::socket::raw::PacketBuffer::new(
                vec![RawPacketMetadata::EMPTY; 4],
                vec![0; RAW_TX_BUF],
            );
            let sock = RawSocket::new(Some(smoltcp::wire::IpVersion::Ipv4), None, rx, tx);
            Ok(sockets.add(sock))
        }
        _ => Err(()),
    }
}

// ── smoltcp Device bridge ───────────────────────────────────────────────────

struct NetdDevice {
    rx_queue: VecDeque<Vec<u8>>,
    tx_queue: VecDeque<Vec<u8>>,
    own_mac: [u8; 6],
}

impl NetdDevice {
    fn new(own_mac: [u8; 6]) -> Self {
        Self {
            rx_queue: VecDeque::new(),
            tx_queue: VecDeque::new(),
            own_mac,
        }
    }
}

impl Device for NetdDevice {
    type RxToken<'a> = NetdRxToken where Self: 'a;
    type TxToken<'a> = NetdTxToken<'a> where Self: 'a;

    fn receive(&mut self, _now: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.rx_queue.pop_front()?;
        let rx_queue = &mut self.rx_queue;
        let tx_queue = &mut self.tx_queue;
        let own_mac = self.own_mac;
        Some((
            NetdRxToken { frame },
            NetdTxToken {
                rx_queue,
                tx_queue,
                own_mac,
                buf: vec![0u8; ETH_MTU],
            },
        ))
    }

    fn transmit(&mut self, _now: Instant) -> Option<Self::TxToken<'_>> {
        let rx_queue = &mut self.rx_queue;
        let tx_queue = &mut self.tx_queue;
        let own_mac = self.own_mac;
        Some(NetdTxToken {
            rx_queue,
            tx_queue,
            own_mac,
            buf: vec![0u8; ETH_MTU],
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = ETH_MTU;
        caps
    }
}

struct NetdRxToken {
    frame: Vec<u8>,
}

impl RxToken for NetdRxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.frame)
    }
}

struct NetdTxToken<'a> {
    rx_queue: &'a mut VecDeque<Vec<u8>>,
    tx_queue: &'a mut VecDeque<Vec<u8>>,
    own_mac: [u8; 6],
    buf: Vec<u8>,
}

impl<'a> TxToken for NetdTxToken<'a> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(mut self, len: usize, f: F) -> R {
        let write_len = len.min(self.buf.len());
        let result = f(&mut self.buf[..write_len]);
        let mut frame = core::mem::take(&mut self.buf);
        frame.truncate(len);
        if len >= 6 {
            let dst = &frame[0..6];
            if dst == self.own_mac {
                self.rx_queue.push_back(frame);
            } else {
                self.tx_queue.push_back(frame);
            }
        } else {
            self.tx_queue.push_back(frame);
        }
        result
    }
}

// ── host tests (kept from todo 7) ──────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::iface::PollResult;
    use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
    use smoltcp::time::Instant;
    use smoltcp::wire::{EthernetAddress, HardwareAddress};

    struct DummyDevice;

    struct DummyRxToken;
    impl RxToken for DummyRxToken {
        fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
            f(&[])
        }
    }

    struct DummyTxToken;
    impl TxToken for DummyTxToken {
        fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _len: usize, f: F) -> R {
            f(&mut [])
        }
    }

    impl Device for DummyDevice {
        type RxToken<'a> = DummyRxToken where Self: 'a;
        type TxToken<'a> = DummyTxToken where Self: 'a;

        fn receive(&mut self, _now: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
            None
        }

        fn transmit(&mut self, _now: Instant) -> Option<Self::TxToken<'_>> {
            None
        }

        fn capabilities(&self) -> DeviceCapabilities {
            let mut caps = DeviceCapabilities::default();
            caps.medium = Medium::Ethernet;
            caps.max_transmission_unit = 1536;
            caps
        }
    }

    #[test]
    fn interface_poll_once() {
        let hw_addr = HardwareAddress::Ethernet(EthernetAddress::from_bytes(&[
            0x02, 0x00, 0x00, 0x00, 0x00, 0x01,
        ]));
        let config = Config::new(hw_addr);
        let mut device = DummyDevice;
        let mut iface = Interface::new(config, &mut device, Instant::ZERO);
        let mut sockets = SocketSet::new(alloc::vec![]);
        let result = iface.poll(Instant::ZERO, &mut device, &mut sockets);
        assert!(matches!(result, PollResult::None));
    }

    #[test]
    fn netd_device_roundtrip() {
        let mut dev = NetdDevice::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        dev.rx_queue.push_back(vec![0xFF; 64]);

        let now = Instant::ZERO;
        let (rx, _tx) = dev.receive(now).expect("frame available");
        let consumed = rx.consume(|data| data.len());
        assert_eq!(consumed, 64);

        let tx = dev.transmit(now).expect("tx token");
        tx.consume(10, |buf| {
            buf[0] = 0x45;
        });
        assert_eq!(dev.tx_queue.len(), 1);
        assert_eq!(dev.tx_queue.pop_front().unwrap().len(), 10);
    }
}
