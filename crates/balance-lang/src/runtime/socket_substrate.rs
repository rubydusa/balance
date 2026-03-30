//! Raw I/O substrates: Socket (TCP/UDP) and RawSocket (IP-level).
//!
//! Socket uses `polling` crate for proper I/O multiplexing.
//! RawSocket uses `socket2` for real raw IP sockets (requires elevated privileges).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::Duration;

use polling::{Event, Events, PollMode, Poller};

use crate::runtime::event::EventBus;
use crate::runtime::interaction::InteractionKind;
use crate::runtime::substrate::Substrate;
use crate::runtime::value::Value;

/// Handle types for socket management.
enum SocketHandle {
    TcpListener(TcpListener),
    TcpStream(TcpStream),
    Udp(UdpSocket),
}

/// OS-level TCP/UDP socket substrate with proper I/O polling.
pub struct SocketSubstrate {
    instance_name: String,
    event_source: String,
    handles: HashMap<String, SocketHandle>,
    next_handle: u64,
    next_offset: u64,
    poller: Poller,
    key_to_handle: HashMap<usize, String>,
    handle_to_key: HashMap<String, usize>,
    next_poll_key: usize,
}

impl SocketSubstrate {
    pub fn new(instance_name: &str, event_source: &str) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            handles: HashMap::new(),
            next_handle: 0,
            next_offset: 0,
            poller: Poller::new().expect("failed to create poller"),
            key_to_handle: HashMap::new(),
            handle_to_key: HashMap::new(),
            next_poll_key: 0,
        }
    }

    fn alloc_handle(&mut self) -> String {
        let id = self.next_handle;
        self.next_handle += 1;
        format!("sock_{id}")
    }

    fn alloc_poll_key(&mut self) -> usize {
        let key = self.next_poll_key;
        self.next_poll_key += 1;
        key
    }

    fn register_with_poller(&mut self, handle_name: &str) {
        let key = self.alloc_poll_key();
        if let Some(sh) = self.handles.get(handle_name) {
            let raw_fd = match sh {
                SocketHandle::TcpListener(l) => l.as_raw_fd(),
                SocketHandle::TcpStream(s) => s.as_raw_fd(),
                SocketHandle::Udp(s) => s.as_raw_fd(),
            };
            let result = unsafe {
                self.poller.add_with_mode(raw_fd, Event::readable(key), PollMode::Level)
            };
            if result.is_ok() {
                self.key_to_handle.insert(key, handle_name.to_string());
                self.handle_to_key.insert(handle_name.to_string(), key);
            }
        }
    }

    fn deregister_from_poller(&mut self, handle_name: &str) {
        if let Some(key) = self.handle_to_key.remove(handle_name) {
            self.key_to_handle.remove(&key);
            // Deregister the fd from the poller
            if let Some(sh) = self.handles.get(handle_name) {
                let raw_fd = match sh {
                    SocketHandle::TcpListener(l) => l.as_raw_fd(),
                    SocketHandle::TcpStream(s) => s.as_raw_fd(),
                    SocketHandle::Udp(s) => s.as_raw_fd(),
                };
                // SAFETY: fd is still valid, we haven't closed it yet
                let borrowed = unsafe { BorrowedFd::borrow_raw(raw_fd) };
                self.poller.delete(borrowed).ok();
            }
        }
    }
}

impl Substrate for SocketSubstrate {
    fn name(&self) -> &str {
        &self.instance_name
    }

    fn event_source(&self) -> &str {
        &self.event_source
    }

    fn execute_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String> {
        match op {
            "tcp_bind" => {
                let addr = get_string_arg(&args, 0, "tcp_bind", "addr")?;
                let listener = TcpListener::bind(&addr)
                    .map_err(|e| format!("tcp_bind failed: {e}"))?;
                let handle = self.alloc_handle();
                let mut data = HashMap::new();
                data.insert("handle".to_string(), Value::String(handle.clone()));
                data.insert("addr".to_string(), Value::String(addr));
                data.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "socket_bound".to_string(), data);
                self.next_offset += 1;
                self.handles.insert(handle.clone(), SocketHandle::TcpListener(listener));
                self.register_with_poller(&handle);
                Ok(Value::String(handle))
            }
            "tcp_connect" => {
                let addr = get_string_arg(&args, 0, "tcp_connect", "addr")?;
                let stream = TcpStream::connect(&addr)
                    .map_err(|e| format!("tcp_connect failed: {e}"))?;
                let handle = self.alloc_handle();
                let mut data = HashMap::new();
                data.insert("handle".to_string(), Value::String(handle.clone()));
                data.insert("addr".to_string(), Value::String(addr));
                data.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "socket_connected".to_string(), data);
                self.next_offset += 1;
                self.handles.insert(handle.clone(), SocketHandle::TcpStream(stream));
                self.register_with_poller(&handle);
                Ok(Value::String(handle))
            }
            "tcp_accept" => {
                let listener_handle = get_string_arg(&args, 0, "tcp_accept", "listener")?;
                let listener = match self.handles.get(&listener_handle) {
                    Some(SocketHandle::TcpListener(l)) => l,
                    _ => return Err(format!("tcp_accept: invalid listener handle '{listener_handle}'")),
                };
                let (stream, addr) = listener.accept()
                    .map_err(|e| format!("tcp_accept failed: {e}"))?;
                let handle = self.alloc_handle();
                let mut data = HashMap::new();
                data.insert("handle".to_string(), Value::String(handle.clone()));
                data.insert("addr".to_string(), Value::String(addr.to_string()));
                data.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "socket_connected".to_string(), data);
                self.next_offset += 1;
                self.handles.insert(handle.clone(), SocketHandle::TcpStream(stream));
                self.register_with_poller(&handle);
                Ok(Value::String(handle))
            }
            "udp_bind" => {
                let addr = get_string_arg(&args, 0, "udp_bind", "addr")?;
                let socket = UdpSocket::bind(&addr)
                    .map_err(|e| format!("udp_bind failed: {e}"))?;
                let handle = self.alloc_handle();
                let mut data = HashMap::new();
                data.insert("handle".to_string(), Value::String(handle.clone()));
                data.insert("addr".to_string(), Value::String(addr));
                data.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "socket_bound".to_string(), data);
                self.next_offset += 1;
                self.handles.insert(handle.clone(), SocketHandle::Udp(socket));
                self.register_with_poller(&handle);
                Ok(Value::String(handle))
            }
            "udp_send_to" => {
                let handle_name = get_string_arg(&args, 0, "udp_send_to", "handle")?;
                let data_bytes = get_bytes_arg(&args, 1, "udp_send_to", "data")?;
                let addr = get_string_arg(&args, 2, "udp_send_to", "addr")?;
                let socket = match self.handles.get(&handle_name) {
                    Some(SocketHandle::Udp(s)) => s,
                    _ => return Err(format!("udp_send_to: invalid handle '{handle_name}'")),
                };
                let sent = socket.send_to(&data_bytes, &addr)
                    .map_err(|e| format!("udp_send_to failed: {e}"))?;
                let mut ev = HashMap::new();
                ev.insert("handle".to_string(), Value::String(handle_name));
                ev.insert("bytes".to_string(), Value::Int(sent as i64));
                ev.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "data_sent".to_string(), ev);
                self.next_offset += 1;
                Ok(Value::Int(sent as i64))
            }
            "udp_recv_from" => {
                let handle_name = get_string_arg(&args, 0, "udp_recv_from", "handle")?;
                let max = get_int_arg(&args, 1, "udp_recv_from", "max")?;
                let socket = match self.handles.get(&handle_name) {
                    Some(SocketHandle::Udp(s)) => s,
                    _ => return Err(format!("udp_recv_from: invalid handle '{handle_name}'")),
                };
                let mut buf = vec![0u8; max as usize];
                let (n, _addr) = socket.recv_from(&mut buf)
                    .map_err(|e| format!("udp_recv_from failed: {e}"))?;
                buf.truncate(n);
                let mut ev = HashMap::new();
                ev.insert("handle".to_string(), Value::String(handle_name));
                ev.insert("bytes".to_string(), Value::Int(n as i64));
                ev.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "data_received".to_string(), ev);
                self.next_offset += 1;
                Ok(Value::Bytes(buf))
            }
            "send" => {
                let handle_name = get_string_arg(&args, 0, "send", "handle")?;
                let data_bytes = get_bytes_arg(&args, 1, "send", "data")?;
                let stream = match self.handles.get_mut(&handle_name) {
                    Some(SocketHandle::TcpStream(s)) => s,
                    _ => return Err(format!("send: invalid stream handle '{handle_name}'")),
                };
                let sent = stream.write(&data_bytes)
                    .map_err(|e| format!("send failed: {e}"))?;
                let _ = stream.flush();
                let mut ev = HashMap::new();
                ev.insert("handle".to_string(), Value::String(handle_name));
                ev.insert("bytes".to_string(), Value::Int(sent as i64));
                ev.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "data_sent".to_string(), ev);
                self.next_offset += 1;
                Ok(Value::Int(sent as i64))
            }
            "recv" => {
                let handle_name = get_string_arg(&args, 0, "recv", "handle")?;
                let max = get_int_arg(&args, 1, "recv", "max")?;
                let stream = match self.handles.get_mut(&handle_name) {
                    Some(SocketHandle::TcpStream(s)) => s,
                    _ => return Err(format!("recv: invalid stream handle '{handle_name}'")),
                };
                let mut buf = vec![0u8; max as usize];
                let n = stream.read(&mut buf)
                    .map_err(|e| format!("recv failed: {e}"))?;
                buf.truncate(n);
                let mut ev = HashMap::new();
                ev.insert("handle".to_string(), Value::String(handle_name));
                ev.insert("bytes".to_string(), Value::Int(n as i64));
                ev.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "data_received".to_string(), ev);
                self.next_offset += 1;
                Ok(Value::Bytes(buf))
            }
            "close" => {
                let handle_name = get_string_arg(&args, 0, "close", "handle")?;
                self.deregister_from_poller(&handle_name);
                self.handles.remove(&handle_name);
                let mut ev = HashMap::new();
                ev.insert("handle".to_string(), Value::String(handle_name.clone()));
                ev.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "socket_closed".to_string(), ev);
                self.next_offset += 1;
                Ok(Value::String(handle_name))
            }
            "poll" => {
                let handles_val = match args.first() {
                    Some(Value::List(l)) => l.clone(),
                    _ => return Err("poll requires a List<String> of handles".to_string()),
                };
                let timeout_ms = get_int_arg(&args, 1, "poll", "timeout_ms")?;

                // Collect the set of handle names we're polling for
                let watching: std::collections::HashSet<String> = handles_val
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();

                let mut events = Events::new();
                let timeout = if timeout_ms > 0 {
                    Some(Duration::from_millis(timeout_ms as u64))
                } else if timeout_ms == 0 {
                    Some(Duration::ZERO)
                } else {
                    // Negative timeout = block indefinitely
                    None
                };

                self.poller.wait(&mut events, timeout)
                    .map_err(|e| format!("poll failed: {e}"))?;

                let ready: Vec<Value> = events
                    .iter()
                    .filter_map(|e| self.key_to_handle.get(&e.key))
                    .filter(|name| watching.contains(*name))
                    .map(|name| Value::String(name.clone()))
                    .collect();

                Ok(Value::List(ready))
            }
            "setsockopt" => {
                let handle_name = get_string_arg(&args, 0, "setsockopt", "handle")?;
                let option = get_string_arg(&args, 1, "setsockopt", "option")?;
                let sh = self.handles.get(&handle_name)
                    .ok_or_else(|| format!("setsockopt: unknown handle '{handle_name}'"))?;
                let raw_fd = match sh {
                    SocketHandle::TcpListener(l) => l.as_raw_fd(),
                    SocketHandle::TcpStream(s) => s.as_raw_fd(),
                    SocketHandle::Udp(s) => s.as_raw_fd(),
                };
                // SAFETY: raw_fd is valid, we still own the handle
                let borrowed = unsafe { BorrowedFd::borrow_raw(raw_fd) };
                let sock_ref = socket2::SockRef::from(&borrowed);
                match option.as_str() {
                    "SO_REUSEADDR" => {
                        let val = match args.get(2) {
                            Some(Value::Bool(b)) => *b,
                            _ => return Err("SO_REUSEADDR requires a Bool value".to_string()),
                        };
                        sock_ref.set_reuse_address(val).map_err(|e| format!("setsockopt: {e}"))?;
                    }
                    "TCP_NODELAY" => {
                        let val = match args.get(2) {
                            Some(Value::Bool(b)) => *b,
                            _ => return Err("TCP_NODELAY requires a Bool value".to_string()),
                        };
                        sock_ref.set_nodelay(val).map_err(|e| format!("setsockopt: {e}"))?;
                    }
                    "SO_RCVBUF" => {
                        let val = match args.get(2) {
                            Some(Value::Int(n)) => *n as usize,
                            _ => return Err("SO_RCVBUF requires an Int value".to_string()),
                        };
                        sock_ref.set_recv_buffer_size(val).map_err(|e| format!("setsockopt: {e}"))?;
                    }
                    "SO_SNDBUF" => {
                        let val = match args.get(2) {
                            Some(Value::Int(n)) => *n as usize,
                            _ => return Err("SO_SNDBUF requires an Int value".to_string()),
                        };
                        sock_ref.set_send_buffer_size(val).map_err(|e| format!("setsockopt: {e}"))?;
                    }
                    "NONBLOCKING" => {
                        let val = match args.get(2) {
                            Some(Value::Bool(b)) => *b,
                            _ => return Err("NONBLOCKING requires a Bool value".to_string()),
                        };
                        sock_ref.set_nonblocking(val).map_err(|e| format!("setsockopt: {e}"))?;
                    }
                    _ => return Err(format!("unknown socket option: {option}")),
                }
                Ok(Value::Unit)
            }
            _ => Err(format!("Socket: unknown op '{op}'")),
        }
    }

    fn guarantees(&self) -> &[String] {
        &[]
    }

    fn op_kind(&self, op: &str) -> InteractionKind {
        match op {
            "tcp_bind" | "tcp_connect" | "tcp_accept" | "udp_bind" | "send" | "close"
            | "udp_send_to" | "setsockopt" => InteractionKind::Command,
            "recv" | "udp_recv_from" | "poll" => InteractionKind::Command,
            _ => InteractionKind::Pure,
        }
    }

    fn set_next_offset(&mut self, offset: u64) {
        self.next_offset = offset;
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Raw IP socket substrate using socket2 for real raw sockets.
/// Requires elevated privileges (root/CAP_NET_RAW on Linux, root on macOS).
pub struct RawSocketSubstrate {
    instance_name: String,
    event_source: String,
    next_offset: u64,
    handles: HashMap<String, socket2::Socket>,
    next_handle: u64,
    poller: Poller,
    key_to_handle: HashMap<usize, String>,
    handle_to_key: HashMap<String, usize>,
    next_poll_key: usize,
}

impl RawSocketSubstrate {
    pub fn new(instance_name: &str, event_source: &str) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            next_offset: 0,
            handles: HashMap::new(),
            next_handle: 0,
            poller: Poller::new().expect("failed to create raw socket poller"),
            key_to_handle: HashMap::new(),
            handle_to_key: HashMap::new(),
            next_poll_key: 0,
        }
    }

    fn alloc_handle(&mut self) -> String {
        let id = self.next_handle;
        self.next_handle += 1;
        format!("raw_{id}")
    }

    fn alloc_poll_key(&mut self) -> usize {
        let key = self.next_poll_key;
        self.next_poll_key += 1;
        key
    }

    fn register_with_poller(&mut self, handle_name: &str) {
        let key = self.alloc_poll_key();
        if let Some(sock) = self.handles.get(handle_name) {
            let raw_fd = sock.as_raw_fd();
            let result = unsafe {
                self.poller.add_with_mode(raw_fd, Event::readable(key), PollMode::Level)
            };
            if result.is_ok() {
                self.key_to_handle.insert(key, handle_name.to_string());
                self.handle_to_key.insert(handle_name.to_string(), key);
            }
        }
    }

    fn deregister_from_poller(&mut self, handle_name: &str) {
        if let Some(key) = self.handle_to_key.remove(handle_name) {
            self.key_to_handle.remove(&key);
            if let Some(sock) = self.handles.get(handle_name) {
                let raw_fd = sock.as_raw_fd();
                let borrowed = unsafe { BorrowedFd::borrow_raw(raw_fd) };
                self.poller.delete(borrowed).ok();
            }
        }
    }
}

impl Substrate for RawSocketSubstrate {
    fn name(&self) -> &str {
        &self.instance_name
    }

    fn event_source(&self) -> &str {
        &self.event_source
    }

    fn execute_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String> {
        match op {
            "open" => {
                let protocol = get_int_arg(&args, 0, "open", "protocol")?;
                let socket = socket2::Socket::new(
                    socket2::Domain::IPV4,
                    socket2::Type::RAW,
                    Some(socket2::Protocol::from(protocol as i32)),
                )
                .map_err(|e| {
                    format!(
                        "raw socket open failed (may require elevated privileges): {e}"
                    )
                })?;
                let handle = self.alloc_handle();
                let mut data = HashMap::new();
                data.insert("handle".to_string(), Value::String(handle.clone()));
                data.insert("protocol".to_string(), Value::Int(protocol));
                data.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "raw_opened".to_string(), data);
                self.next_offset += 1;
                self.handles.insert(handle.clone(), socket);
                self.register_with_poller(&handle);
                Ok(Value::String(handle))
            }
            "bind" => {
                let handle_name = get_string_arg(&args, 0, "bind", "handle")?;
                let addr = get_string_arg(&args, 1, "bind", "addr")?;
                let socket = self.handles.get(&handle_name)
                    .ok_or_else(|| format!("bind: invalid handle '{handle_name}'"))?;
                let sock_addr: std::net::SocketAddr = addr.parse()
                    .map_err(|e| format!("bind: invalid address '{addr}': {e}"))?;
                socket
                    .bind(&socket2::SockAddr::from(sock_addr))
                    .map_err(|e| format!("bind failed: {e}"))?;
                Ok(Value::String(handle_name))
            }
            "send_to" => {
                let handle_name = get_string_arg(&args, 0, "send_to", "handle")?;
                let data_bytes = get_bytes_arg(&args, 1, "send_to", "data")?;
                let addr = get_string_arg(&args, 2, "send_to", "addr")?;
                let socket = self.handles.get(&handle_name)
                    .ok_or_else(|| format!("send_to: invalid handle '{handle_name}'"))?;
                let sock_addr: std::net::SocketAddr = addr.parse()
                    .map_err(|e| format!("send_to: invalid address '{addr}': {e}"))?;
                let sent = socket
                    .send_to(&data_bytes, &socket2::SockAddr::from(sock_addr))
                    .map_err(|e| format!("send_to failed: {e}"))?;
                let mut ev = HashMap::new();
                ev.insert("handle".to_string(), Value::String(handle_name));
                ev.insert("bytes".to_string(), Value::Int(sent as i64));
                ev.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "raw_sent".to_string(), ev);
                self.next_offset += 1;
                Ok(Value::Int(sent as i64))
            }
            "recv_from" => {
                let handle_name = get_string_arg(&args, 0, "recv_from", "handle")?;
                let max = get_int_arg(&args, 1, "recv_from", "max")?;
                let socket = self.handles.get(&handle_name)
                    .ok_or_else(|| format!("recv_from: invalid handle '{handle_name}'"))?;
                let mut buf = vec![std::mem::MaybeUninit::uninit(); max as usize];
                let (n, _addr) = socket
                    .recv_from(&mut buf)
                    .map_err(|e| format!("recv_from failed: {e}"))?;
                let received: Vec<u8> = buf[..n]
                    .iter()
                    .map(|b| unsafe { b.assume_init() })
                    .collect();
                let mut ev = HashMap::new();
                ev.insert("handle".to_string(), Value::String(handle_name));
                ev.insert("bytes".to_string(), Value::Int(n as i64));
                ev.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "raw_received".to_string(), ev);
                self.next_offset += 1;
                Ok(Value::Bytes(received))
            }
            "poll" => {
                let handles_val = match args.first() {
                    Some(Value::List(l)) => l.clone(),
                    _ => return Err("poll requires a List<String> of handles".to_string()),
                };
                let timeout_ms = get_int_arg(&args, 1, "poll", "timeout_ms")?;

                let watching: std::collections::HashSet<String> = handles_val
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();

                let mut events = Events::new();
                let timeout = if timeout_ms > 0 {
                    Some(Duration::from_millis(timeout_ms as u64))
                } else if timeout_ms == 0 {
                    Some(Duration::ZERO)
                } else {
                    None
                };

                self.poller.wait(&mut events, timeout)
                    .map_err(|e| format!("poll failed: {e}"))?;

                let ready: Vec<Value> = events
                    .iter()
                    .filter_map(|e| self.key_to_handle.get(&e.key))
                    .filter(|name| watching.contains(*name))
                    .map(|name| Value::String(name.clone()))
                    .collect();

                Ok(Value::List(ready))
            }
            "close" => {
                let handle_name = get_string_arg(&args, 0, "close", "handle")?;
                self.deregister_from_poller(&handle_name);
                self.handles.remove(&handle_name);
                let mut ev = HashMap::new();
                ev.insert("handle".to_string(), Value::String(handle_name.clone()));
                ev.insert("offset".to_string(), Value::Int(self.next_offset as i64));
                event_bus.publish(self.event_source.clone(), "raw_closed".to_string(), ev);
                self.next_offset += 1;
                Ok(Value::String(handle_name))
            }
            "setsockopt" => {
                let handle_name = get_string_arg(&args, 0, "setsockopt", "handle")?;
                let option = get_string_arg(&args, 1, "setsockopt", "option")?;
                let sock = self.handles.get(&handle_name)
                    .ok_or_else(|| format!("setsockopt: unknown handle '{handle_name}'"))?;
                match option.as_str() {
                    "IP_HDRINCL" => {
                        let val = match args.get(2) {
                            Some(Value::Bool(b)) => *b,
                            _ => return Err("IP_HDRINCL requires a Bool value".to_string()),
                        };
                        sock.set_header_included_v4(val).map_err(|e| format!("setsockopt: {e}"))?;
                    }
                    "SO_REUSEADDR" => {
                        let val = match args.get(2) {
                            Some(Value::Bool(b)) => *b,
                            _ => return Err("SO_REUSEADDR requires a Bool value".to_string()),
                        };
                        sock.set_reuse_address(val).map_err(|e| format!("setsockopt: {e}"))?;
                    }
                    "NONBLOCKING" => {
                        let val = match args.get(2) {
                            Some(Value::Bool(b)) => *b,
                            _ => return Err("NONBLOCKING requires a Bool value".to_string()),
                        };
                        sock.set_nonblocking(val).map_err(|e| format!("setsockopt: {e}"))?;
                    }
                    _ => return Err(format!("unknown raw socket option: {option}")),
                }
                Ok(Value::Unit)
            }
            _ => Err(format!("RawSocket: unknown op '{op}'")),
        }
    }

    fn guarantees(&self) -> &[String] {
        &[]
    }

    fn op_kind(&self, op: &str) -> InteractionKind {
        match op {
            "open" | "bind" | "send_to" | "close" | "setsockopt" => InteractionKind::Command,
            "recv_from" | "poll" => InteractionKind::Command,
            _ => InteractionKind::Command,
        }
    }

    fn set_next_offset(&mut self, offset: u64) {
        self.next_offset = offset;
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

// ─── Helper functions ──────────────────────────────────────────────────────

fn get_string_arg(args: &[Value], idx: usize, op: &str, name: &str) -> Result<String, String> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(format!("{op}: argument '{name}' must be a String")),
    }
}

fn get_int_arg(args: &[Value], idx: usize, op: &str, name: &str) -> Result<i64, String> {
    match args.get(idx) {
        Some(Value::Int(n)) => Ok(*n),
        _ => Err(format!("{op}: argument '{name}' must be an Int")),
    }
}

fn get_bytes_arg(args: &[Value], idx: usize, op: &str, name: &str) -> Result<Vec<u8>, String> {
    match args.get(idx) {
        Some(Value::Bytes(b)) => Ok(b.clone()),
        _ => Err(format!("{op}: argument '{name}' must be Bytes")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_udp_loopback() {
        let mut sock = SocketSubstrate::new("sock/test", "sock/events");
        let mut event_bus = crate::runtime::event::EventBus::new();

        // Bind UDP socket
        let handle = sock.execute_op(
            "udp_bind",
            vec![Value::String("127.0.0.1:0".to_string())],
            &mut event_bus,
        ).unwrap();

        if let Value::String(h) = &handle {
            // Get the actual bound address
            if let Some(SocketHandle::Udp(s)) = sock.handles.get(h) {
                let addr = s.local_addr().unwrap().to_string();

                // Send data to ourselves
                sock.execute_op(
                    "udp_send_to",
                    vec![
                        handle.clone(),
                        Value::Bytes(vec![1, 2, 3]),
                        Value::String(addr),
                    ],
                    &mut event_bus,
                ).unwrap();

                // Receive data
                let recv = sock.execute_op(
                    "udp_recv_from",
                    vec![handle.clone(), Value::Int(1024)],
                    &mut event_bus,
                ).unwrap();
                assert_eq!(recv, Value::Bytes(vec![1, 2, 3]));
            }

            // Close
            sock.execute_op("close", vec![handle], &mut event_bus).unwrap();
        }
    }

    #[test]
    fn test_socket_poll_udp() {
        let mut sock = SocketSubstrate::new("sock/poll", "sock/events");
        let mut event_bus = crate::runtime::event::EventBus::new();

        // Bind UDP socket
        let handle = sock.execute_op(
            "udp_bind",
            vec![Value::String("127.0.0.1:0".to_string())],
            &mut event_bus,
        ).unwrap();

        if let Value::String(h) = &handle {
            if let Some(SocketHandle::Udp(s)) = sock.handles.get(h) {
                let addr = s.local_addr().unwrap().to_string();

                // Send data to ourselves
                sock.execute_op(
                    "udp_send_to",
                    vec![
                        handle.clone(),
                        Value::Bytes(vec![42]),
                        Value::String(addr),
                    ],
                    &mut event_bus,
                ).unwrap();

                // Poll should show it as ready
                let ready = sock.execute_op(
                    "poll",
                    vec![
                        Value::List(vec![handle.clone()]),
                        Value::Int(1000),
                    ],
                    &mut event_bus,
                ).unwrap();

                if let Value::List(ready_list) = &ready {
                    assert!(!ready_list.is_empty(), "poll should report socket as ready");
                }
            }

            sock.execute_op("close", vec![handle], &mut event_bus).unwrap();
        }
    }
}
