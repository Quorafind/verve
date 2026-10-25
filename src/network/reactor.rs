//! One mio poller per realm. Read credits bound queued plaintext to one chunk per socket;
//! listeners likewise accept only after the preceding accept has reached JavaScript.
use mio::{
    Events, Interest, Poll, Token, Waker,
    net::{TcpListener, TcpStream, UdpSocket},
};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) type Id = u64;
pub(crate) enum Command {
    Connect(Id, String, u16, u8),
    /// A connection to a target through a proxy: the target is what permissions are checked
    /// against, and it is never resolved here.
    ConnectVia(Id, String, u16, super::proxy::Via),
    /// The proxies the embedder configured, which need no outbound rule of their own.
    Proxies(Vec<(String, u16)>),
    Listen(Id, SocketAddr, i32, bool),
    Accept(Id),
    Adopt(Id, Id),
    Read(Id, bool),
    Write(Id, Vec<u8>),
    End(Id),
    Close(Id),
    NoDelay(Id, bool),
    KeepAlive(Id, bool, u64),
    Timeout(Id, u64),
    Dns(Id, super::dns::Query),
    DnsResult(Id, Result<super::dns::Answer, String>),
    TlsClient(Id, super::tls::Options),
    TlsServer(Id, super::tls::Options),
    TlsAuthorize(Id),
    UdpBind(Id, SocketAddr, bool),
    UdpSend(Id, Id, String, u16, Vec<u8>),
    UdpConnect(Id, Id, String, u16),
    UdpOption(Id, String, String, String),
    Stop,
}
#[derive(Debug)]
pub(crate) enum Event {
    Connected(SocketAddr, SocketAddr),
    Listening(SocketAddr),
    Accepted(Id, SocketAddr, SocketAddr),
    Data(Vec<u8>),
    Written(usize),
    Finished,
    End,
    Closed,
    Timeout,
    Secure(super::tls::SessionInfo),
    Dns(Result<super::dns::Answer, String>),
    Datagram(Vec<u8>, SocketAddr),
    DatagramSent(Id, Result<usize, String>),
    DatagramConnected(SocketAddr, SocketAddr),
    DatagramBuffers(usize, usize),
    Error(String, String),
    /// The proxy answered a `CONNECT` with this status rather than 200.
    TunnelRefused(u16, String),
}

#[derive(Clone)]
pub(crate) struct Sender {
    tx: mpsc::Sender<Command>,
    wake: Arc<Waker>,
}
impl Sender {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
        let _ = self.wake.wake();
    }
}
#[derive(Default)]
pub(crate) struct Counters {
    pub reactor_threads: AtomicUsize,
    pub dns_threads: AtomicUsize,
    pub open_sockets: AtomicUsize,
    pub queued_read_bytes: AtomicUsize,
    stopped: AtomicBool,
}
struct ThreadCount<'a>(&'a AtomicUsize);
impl<'a> ThreadCount<'a> {
    fn new(value: &'a AtomicUsize) -> Self {
        value.fetch_add(1, Ordering::Relaxed);
        Self(value)
    }
}
impl Drop for ThreadCount<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
struct Output {
    tx: mpsc::Sender<(Id, Event)>,
    counters: Arc<Counters>,
    wake: Arc<crate::WakeSlot>,
}
impl Output {
    fn send(&self, event: (Id, Event)) -> Result<(), ()> {
        let n = match &event.1 {
            Event::Data(bytes) | Event::Datagram(bytes, _) => bytes.len(),
            _ => 0,
        };
        self.counters
            .queued_read_bytes
            .fetch_add(n, Ordering::Relaxed);
        let result = self.tx.send(event).map_err(|_| ());
        if result.is_ok() {
            self.wake.wake();
        } else {
            self.counters
                .queued_read_bytes
                .fetch_sub(n, Ordering::Relaxed);
        }
        result
    }
}
pub(crate) struct Reactor {
    pub events: mpsc::Receiver<(Id, Event)>,
    pub sender: Sender,
    pub counters: Arc<Counters>,
}
impl Reactor {
    pub fn new(
        permissions: super::Permissions,
        proxies: Vec<(String, u16)>,
        host: Arc<crate::WakeSlot>,
    ) -> io::Result<Self> {
        let poll = Poll::new()?;
        let wake = Arc::new(Waker::new(poll.registry(), Token(0))?);
        let (tx, commands) = mpsc::channel();
        let (out, events) = mpsc::channel();
        let counters = Arc::new(Counters::default());
        let out = Output {
            tx: out,
            counters: counters.clone(),
            wake: host,
        };
        let sender = Sender { tx, wake };
        let resolver = sender.clone();
        let (dns_tx, dns_rx) = mpsc::sync_channel::<(Id, super::dns::Query)>(1024);
        let dns_rx = Arc::new(Mutex::new(dns_rx));
        for n in 0..2 {
            let rx = dns_rx.clone();
            let sender = resolver.clone();
            let counters = counters.clone();
            thread::Builder::new()
                .name(format!("verve-connect-dns-{n}"))
                .spawn(move || {
                    let _count = ThreadCount::new(&counters.dns_threads);
                    loop {
                        if counters.stopped.load(Ordering::Acquire) {
                            break;
                        }
                        let job = match rx.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => break,
                        };
                        let Ok((id, query)) = job else {
                            break;
                        };
                        if counters.stopped.load(Ordering::Acquire) {
                            break;
                        }
                        sender.send(Command::DnsResult(id, super::dns::resolve(query)));
                    }
                })?;
        }
        thread::Builder::new()
            .name("verve-network".into())
            .spawn(move || {
                let counters = out.counters.clone();
                let _count = ThreadCount::new(&counters.reactor_threads);
                run(poll, commands, out, dns_tx, permissions, proxies);
            })?;
        Ok(Self {
            events,
            sender,
            counters,
        })
    }
    pub fn send(&self, command: Command) {
        self.sender.send(command);
    }
}
impl Drop for Reactor {
    fn drop(&mut self) {
        self.counters.stopped.store(true, Ordering::Release);
        self.sender.send(Command::Stop);
    }
}

struct Connection {
    socket: TcpStream,
    connecting: bool,
    credit: bool,
    registered: bool,
    writes: VecDeque<(Vec<u8>, usize)>,
    end: bool,
    ended: bool,
    timeout: u64,
    activity: Instant,
    alternatives: VecDeque<SocketAddr>,
    tls: Option<rustls::Connection>,
    secured: bool,
    tls_written: Option<usize>,
    finishing: bool,
    failed: bool,
    tls_permitted: bool,
    tls_server: bool,
    tls_verification: Arc<Mutex<Option<String>>>,
    /// A `CONNECT` not yet answered: nothing else is read or written until it is.
    tunnel: Option<Tunnel>,
}
/// A `CONNECT` request on its way to the proxy, and the proxy's answer as it arrives.
struct Tunnel {
    request: Vec<u8>,
    sent: usize,
    response: Vec<u8>,
    target: String,
}
impl Tunnel {
    fn new(host: &str, port: u16, authorization: Option<&str>) -> Self {
        let target = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some(credentials) = authorization {
            request.push_str(&format!("Proxy-Authorization: {credentials}\r\n"));
        }
        request.push_str("\r\n");
        Self {
            request: request.into_bytes(),
            sent: 0,
            response: Vec::new(),
            target,
        }
    }
}
/// Where a tunnel stands after a step.
enum Tunneled {
    Waiting,
    Open,
    Refused(u16, String),
}
/// Sends what is left of the `CONNECT` and reads the answer's head, a byte at a time so that
/// nothing after it — the target's first bytes — is taken from the socket.
fn tunnel_step(c: &mut Connection) -> io::Result<Tunneled> {
    let Some(t) = c.tunnel.as_mut() else {
        return Ok(Tunneled::Open);
    };
    while t.sent < t.request.len() {
        match c.socket.write(&t.request[t.sent..]) {
            Ok(n) => t.sent += n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Tunneled::Waiting),
            Err(e) => return Err(e),
        }
    }
    let mut byte = [0; 1];
    while !t.response.ends_with(b"\r\n\r\n") {
        if t.response.len() > 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the proxy's answer to CONNECT is too long",
            ));
        }
        match c.socket.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    format!(
                        "the proxy closed the connection before tunneling to {}",
                        t.target
                    ),
                ));
            }
            Ok(_) => t.response.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Tunneled::Waiting),
            Err(e) => return Err(e),
        }
    }
    c.activity = Instant::now();
    let head = String::from_utf8_lossy(&t.response);
    let line = head.lines().next().unwrap_or_default().trim().to_owned();
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|_| line.starts_with("HTTP/1."));
    match status {
        Some(200) => {
            c.tunnel = None;
            Ok(Tunneled::Open)
        }
        Some(status) => Ok(Tunneled::Refused(
            status,
            format!(
                "Failed to establish tunnel to {} through the proxy: {line}",
                t.target
            ),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the proxy answered CONNECT with something not HTTP: {line}"),
        )),
    }
}
impl Connection {
    fn new(socket: TcpStream, connecting: bool) -> Self {
        Self {
            socket,
            connecting,
            credit: false,
            registered: false,
            writes: VecDeque::new(),
            end: false,
            ended: false,
            timeout: 0,
            activity: Instant::now(),
            alternatives: VecDeque::new(),
            tls: None,
            secured: false,
            tls_written: None,
            finishing: false,
            failed: false,
            tls_permitted: false,
            tls_server: false,
            tls_verification: Arc::default(),
            tunnel: None,
        }
    }
    fn interest(&self) -> Option<Interest> {
        if self.failed {
            return None;
        }
        if let Some(t) = &self.tunnel
            && !self.connecting
        {
            return Some(if t.sent < t.request.len() {
                Interest::READABLE | Interest::WRITABLE
            } else {
                Interest::READABLE
            });
        }
        match (
            (self.credit && !self.ended && (self.tls.is_none() || self.tls_permitted))
                || self.tls.as_ref().is_some_and(|t| t.is_handshaking()),
            self.connecting
                || (!self.writes.is_empty() && (self.tls.is_none() || self.tls_permitted))
                || self.tls.as_ref().is_some_and(|t| t.wants_write()),
        ) {
            (true, true) => Some(Interest::READABLE | Interest::WRITABLE),
            (true, false) => Some(Interest::READABLE),
            (false, true) => Some(Interest::WRITABLE),
            _ => None,
        }
    }
    fn register(&mut self, poll: &Poll, id: Id) -> io::Result<()> {
        match self.interest() {
            Some(interest) if self.registered => {
                poll.registry()
                    .reregister(&mut self.socket, Token(id as usize), interest)
            }
            Some(interest) => {
                poll.registry()
                    .register(&mut self.socket, Token(id as usize), interest)?;
                self.registered = true;
                Ok(())
            }
            None if self.registered => {
                poll.registry().deregister(&mut self.socket)?;
                self.registered = false;
                Ok(())
            }
            None => Ok(()),
        }
    }
}
struct Datagram {
    socket: UdpSocket,
    registered: bool,
    credit: bool,
    inbound: bool,
    peers: std::collections::HashSet<SocketAddr>,
    writes: VecDeque<(Id, SocketAddr, Vec<u8>)>,
}
struct UdpJob {
    socket: Id,
    bytes: Option<Vec<u8>>,
}
struct Listener {
    socket: TcpListener,
    registered: bool,
}
fn code(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::ConnectionRefused => "ECONNREFUSED",
        io::ErrorKind::ConnectionReset => "ECONNRESET",
        io::ErrorKind::ConnectionAborted => "ECONNABORTED",
        io::ErrorKind::AddrInUse => "EADDRINUSE",
        io::ErrorKind::AddrNotAvailable => "EADDRNOTAVAIL",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::TimedOut => "ETIMEDOUT",
        io::ErrorKind::BrokenPipe => "EPIPE",
        _ => "EIO",
    }
}
fn error(out: &Output, id: Id, e: io::Error) {
    let message = e.to_string();
    let tls_code = message
        .split_once(": ")
        .map(|(code, _)| code)
        .filter(|code| {
            code.starts_with("ERR_TLS_")
                || code.starts_with("ERR_SSL_")
                || code.starts_with("CERT_")
                || code.starts_with("UNABLE_TO_")
                || code.starts_with("DEPTH_ZERO_")
                || code.starts_with("SELF_SIGNED_")
        });
    let _ = out.send((
        id,
        Event::Error(tls_code.unwrap_or_else(|| code(&e)).into(), message),
    ));
}
fn bind_listener(address: SocketAddr, backlog: i32, ipv6_only: bool) -> io::Result<TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    if address.is_ipv6() {
        socket.set_only_v6(ipv6_only)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(backlog)?;
    let socket: std::net::TcpListener = socket.into();
    Ok(TcpListener::from_std(socket))
}
fn run(
    mut poll: Poll,
    commands: mpsc::Receiver<Command>,
    out: Output,
    dns: mpsc::SyncSender<(Id, super::dns::Query)>,
    permissions: super::Permissions,
    mut proxies: Vec<(String, u16)>,
) {
    let mut sockets: HashMap<Id, Connection> = HashMap::new();
    let mut tls_options = HashMap::new();
    let mut datagrams: HashMap<Id, Datagram> = HashMap::new();
    let mut udp_jobs: HashMap<Id, UdpJob> = HashMap::new();
    let mut listeners: HashMap<Id, Listener> = HashMap::new();
    let mut pending = HashMap::new();
    let mut accepted: HashMap<Id, (Id, TcpStream)> = HashMap::new();
    let mut accept_id = 1_u64 << 52;
    let mut events = Events::with_capacity(1024);
    loop {
        if out.counters.stopped.load(Ordering::Acquire) {
            return;
        }
        out.counters.open_sockets.store(
            sockets.len() + datagrams.len() + listeners.len() + accepted.len(),
            Ordering::Relaxed,
        );
        for _ in 0..4096 {
            let command = match commands.try_recv() {
                Ok(c) => c,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            };
            match command {
                Command::TlsAuthorize(id) => {
                    if let Some(socket) = sockets.get_mut(&id) {
                        socket.tls_permitted = true;
                    }
                }
                Command::TlsClient(id, options) => {
                    tls_options.insert(id, (options, false));
                }
                Command::TlsServer(id, options) => {
                    tls_options.insert(id, (options, true));
                }
                Command::UdpBind(id, address, inbound) => {
                    if inbound && !permissions.allows_listen(address.ip()) {
                        let _ =
                            out.send((id, Event::Error("EACCES".into(), "listen EACCES".into())));
                        continue;
                    }
                    match UdpSocket::bind(address) {
                        Ok(socket) => {
                            udp_buffers(&out, id, &socket);
                            if let Ok(local) = socket.local_addr() {
                                let _ = out.send((id, Event::Listening(local)));
                            }
                            datagrams.insert(
                                id,
                                Datagram {
                                    socket,
                                    registered: false,
                                    credit: true,
                                    inbound,
                                    peers: std::collections::HashSet::new(),
                                    writes: VecDeque::new(),
                                },
                            );
                        }
                        Err(e) => error(&out, id, e),
                    }
                }
                Command::UdpSend(id, request, host, port, bytes) => {
                    // A connected peer is an OS-owned address, not the mutable JS metadata.
                    if let Some(peer) = datagrams.get(&id).and_then(|d| d.socket.peer_addr().ok()) {
                        if let Some(d) = datagrams.get_mut(&id) {
                            d.writes.push_back((request, peer, bytes));
                        }
                        continue;
                    }
                    if !permissions.allows_outbound(&host, port) {
                        let _ = out.send((id, Event::DatagramSent(request, Err("EACCES".into()))));
                        continue;
                    }
                    if let Ok(ip) = host.parse::<IpAddr>() {
                        if let Some(d) = datagrams.get_mut(&id) {
                            let address = SocketAddr::new(ip, port);
                            d.peers.insert(address);
                            d.writes.push_back((request, address, bytes));
                        }
                    } else {
                        let family = datagrams
                            .get(&id)
                            .and_then(|d| d.socket.local_addr().ok())
                            .map(|a| if a.is_ipv4() { 4 } else { 6 })
                            .unwrap_or(0);
                        udp_jobs.insert(
                            request,
                            UdpJob {
                                socket: id,
                                bytes: Some(bytes),
                            },
                        );
                        if dns
                            .try_send((
                                request,
                                super::dns::Query {
                                    host,
                                    kind: format!("connect:{port}"),
                                    family,
                                    servers: Vec::new(),
                                    ttl: false,
                                },
                            ))
                            .is_err()
                        {
                            udp_jobs.remove(&request);
                            let _ = out
                                .send((id, Event::DatagramSent(request, Err("EAI_AGAIN".into()))));
                        }
                    }
                }
                Command::UdpConnect(id, request, host, port) => {
                    if !permissions.allows_outbound(&host, port) {
                        let _ =
                            out.send((id, Event::Error("EACCES".into(), "connect EACCES".into())));
                        continue;
                    }
                    udp_jobs.insert(
                        request,
                        UdpJob {
                            socket: id,
                            bytes: None,
                        },
                    );
                    let family = datagrams
                        .get(&id)
                        .and_then(|d| d.socket.local_addr().ok())
                        .map(|a| if a.is_ipv4() { 4 } else { 6 })
                        .unwrap_or(0);
                    if dns
                        .try_send((
                            request,
                            super::dns::Query {
                                host,
                                kind: format!("connect:{port}"),
                                family,
                                servers: Vec::new(),
                                ttl: false,
                            },
                        ))
                        .is_err()
                    {
                        udp_jobs.remove(&request);
                        let _ = out.send((
                            id,
                            Event::Error("EAI_AGAIN".into(), "getaddrinfo queue is full".into()),
                        ));
                    }
                }
                Command::UdpOption(id, name, value, extra) => {
                    if let Some(d) = datagrams.get_mut(&id) {
                        match udp_option(&d.socket, &name, &value, &extra) {
                            Ok(()) => udp_buffers(&out, id, &d.socket),
                            Err(e) => error(&out, id, e),
                        }
                    }
                }
                Command::Stop => return,
                Command::Proxies(list) => proxies = list,
                Command::Connect(..) | Command::ConnectVia(..) => {
                    let (id, host, port, family, tunnel) = match command {
                        Command::Connect(id, host, port, family) => {
                            if !permissions.allows_outbound(&host, port) {
                                let _ = out.send((
                                    id,
                                    Event::Error("EACCES".into(), "connect EACCES".into()),
                                ));
                                continue;
                            }
                            (id, host, port, family, None)
                        }
                        Command::ConnectVia(id, host, port, via) => {
                            let proxy = &via.proxy;
                            let configured = proxies.iter().any(|(h, p)| {
                                h.eq_ignore_ascii_case(&proxy.host) && *p == proxy.port
                            });
                            if !permissions.allows_outbound(&host, port)
                                || !(configured
                                    || permissions.allows_outbound(&proxy.host, proxy.port))
                            {
                                let _ = out.send((
                                    id,
                                    Event::Error("EACCES".into(), "connect EACCES".into()),
                                ));
                                continue;
                            }
                            let tunnel = via
                                .tunnel
                                .then(|| Tunnel::new(&host, port, proxy.authorization.as_deref()));
                            (id, via.proxy.host, via.proxy.port, 0, tunnel)
                        }
                        _ => continue,
                    };
                    pending.insert(
                        id,
                        Waiting {
                            tunnel,
                            ..Waiting::default()
                        },
                    );
                    if let Ok(ip) = host.parse::<IpAddr>() {
                        let result = vec![SocketAddr::new(ip, port)];
                        open(id, result, &mut sockets, &mut pending, &poll, &out);
                    } else if dns
                        .try_send((
                            id,
                            super::dns::Query {
                                host,
                                kind: format!("connect:{port}"),
                                family,
                                servers: Vec::new(),
                                ttl: false,
                            },
                        ))
                        .is_err()
                    {
                        pending.remove(&id);
                        let _ = out.send((
                            id,
                            Event::Error("EAI_AGAIN".into(), "getaddrinfo queue is full".into()),
                        ));
                    }
                }
                Command::Dns(id, query) => {
                    if !permissions.dns {
                        let _ = out.send((id, Event::Dns(Err("EACCES".into()))));
                        continue;
                    }
                    if dns.try_send((id, query)).is_err() {
                        let _ = out.send((id, Event::Dns(Err("EAI_AGAIN".into()))));
                    }
                }
                Command::DnsResult(id, result) => {
                    if let Some(job) = udp_jobs.remove(&id) {
                        let address = match result {
                            Ok(super::dns::Answer::Connect(a)) => a.into_iter().next(),
                            _ => None,
                        };
                        if let Some(address) = address {
                            if let Some(d) = datagrams.get_mut(&job.socket) {
                                d.peers.insert(address);
                                if let Some(bytes) = job.bytes {
                                    d.writes.push_back((id, address, bytes));
                                } else {
                                    match d.socket.connect(address) {
                                        Ok(()) => {
                                            if let Ok(local) = d.socket.local_addr() {
                                                let _ = out.send((
                                                    job.socket,
                                                    Event::DatagramConnected(local, address),
                                                ));
                                            }
                                        }
                                        Err(e) => error(&out, job.socket, e),
                                    }
                                }
                            }
                        } else {
                            let _ = out.send((
                                job.socket,
                                Event::DatagramSent(id, Err("ENOTFOUND".into())),
                            ));
                        }
                        continue;
                    }
                    if pending.contains_key(&id) {
                        match result {
                            Ok(super::dns::Answer::Connect(addresses)) => {
                                open(id, addresses, &mut sockets, &mut pending, &poll, &out)
                            }
                            _ => {
                                pending.remove(&id);
                                let _ = out.send((
                                    id,
                                    Event::Error(
                                        "ENOTFOUND".into(),
                                        "getaddrinfo ENOTFOUND".into(),
                                    ),
                                ));
                            }
                        }
                    } else {
                        let _ = out.send((id, Event::Dns(result)));
                    }
                }
                Command::Listen(id, address, backlog, ipv6_only) => {
                    if !permissions.allows_listen(address.ip()) {
                        let _ =
                            out.send((id, Event::Error("EACCES".into(), "listen EACCES".into())));
                        continue;
                    }
                    match bind_listener(address, backlog, ipv6_only).and_then(|mut socket| {
                        let address = socket.local_addr()?;
                        poll.registry().register(
                            &mut socket,
                            Token(id as usize),
                            Interest::READABLE,
                        )?;
                        Ok((socket, address))
                    }) {
                        Ok((socket, address)) => {
                            listeners.insert(
                                id,
                                Listener {
                                    socket,
                                    registered: true,
                                },
                            );
                            let _ = out.send((id, Event::Listening(address)));
                        }
                        Err(e) => error(&out, id, e),
                    }
                }
                Command::Accept(id) => {
                    if let Some(l) = listeners.get_mut(&id)
                        && !l.registered
                        && poll
                            .registry()
                            .register(&mut l.socket, Token(id as usize), Interest::READABLE)
                            .is_ok()
                    {
                        l.registered = true;
                    }
                }
                Command::Adopt(ticket, id) => {
                    if let Some((_, socket)) = accepted.remove(&ticket) {
                        sockets.insert(id, Connection::new(socket, false));
                    }
                }
                Command::Read(id, enabled) => {
                    if let Some(d) = datagrams.get_mut(&id) {
                        d.credit = enabled;
                    }
                    if let Some(c) = sockets.get_mut(&id) {
                        c.credit = enabled;
                    }
                    if let Some(p) = pending.get_mut(&id) {
                        p.read = enabled;
                    }
                }
                Command::Write(id, data) => {
                    if let Some(c) = sockets.get_mut(&id) {
                        c.writes.push_back((data, 0));
                    } else if let Some(p) = pending.get_mut(&id) {
                        p.writes.push_back((data, 0));
                    }
                }
                Command::End(id) => {
                    if let Some(c) = sockets.get_mut(&id) {
                        c.end = true;
                    }
                    if let Some(p) = pending.get_mut(&id) {
                        p.end = true;
                    }
                }
                Command::NoDelay(id, enabled) => {
                    if let Some(c) = sockets.get(&id)
                        && let Err(e) = c.socket.set_nodelay(enabled)
                    {
                        error(&out, id, e);
                    }
                }
                Command::KeepAlive(id, enabled, delay) => {
                    if let Some(c) = sockets.get(&id) {
                        let sock = socket2::SockRef::from(&c.socket);
                        if let Err(e) = sock.set_keepalive(enabled).and_then(|_| {
                            if enabled {
                                sock.set_tcp_keepalive(
                                    &socket2::TcpKeepalive::new()
                                        .with_time(Duration::from_millis(delay.max(1000))),
                                )
                            } else {
                                Ok(())
                            }
                        }) {
                            error(&out, id, e);
                        }
                    }
                }
                Command::Timeout(id, ms) => {
                    if let Some(c) = sockets.get_mut(&id) {
                        c.timeout = ms;
                        c.activity = Instant::now();
                    }
                    if let Some(p) = pending.get_mut(&id) {
                        p.timeout = ms;
                    }
                }
                Command::Close(id) => {
                    if let Some(mut c) = sockets.remove(&id) {
                        let _ = poll.registry().deregister(&mut c.socket);
                    }
                    if let Some(mut l) = listeners.remove(&id) {
                        let _ = poll.registry().deregister(&mut l.socket);
                    }
                    if let Some(mut d) = datagrams.remove(&id) {
                        let _ = poll.registry().deregister(&mut d.socket);
                    }
                    udp_jobs.retain(|_, job| job.socket != id);
                    tls_options.remove(&id);
                    pending.remove(&id);
                    accepted.remove(&id);
                    accepted.retain(|_, (listener, _)| *listener != id);
                    let _ = out.send((id, Event::Closed));
                }
            }
        }
        for (&id, d) in datagrams.iter_mut() {
            if let Some((request, address, bytes)) = d.writes.front() {
                match d.socket.send_to(bytes, *address) {
                    Ok(n) => {
                        let _ = out.send((id, Event::DatagramSent(*request, Ok(n))));
                        d.writes.pop_front();
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => {
                        let _ = out.send((id, Event::DatagramSent(*request, Err(code(&e).into()))));
                        d.writes.pop_front();
                    }
                }
            }
            let interest = if d.credit {
                Some(if d.writes.is_empty() {
                    Interest::READABLE
                } else {
                    Interest::READABLE | Interest::WRITABLE
                })
            } else if !d.writes.is_empty() {
                Some(Interest::WRITABLE)
            } else {
                None
            };
            let result = match interest {
                Some(i) if d.registered => {
                    poll.registry()
                        .reregister(&mut d.socket, Token(id as usize), i)
                }
                Some(i) => {
                    d.registered = true;
                    poll.registry()
                        .register(&mut d.socket, Token(id as usize), i)
                }
                None if d.registered => {
                    d.registered = false;
                    poll.registry().deregister(&mut d.socket)
                }
                None => Ok(()),
            };
            if let Err(e) = result {
                error(&out, id, e);
            }
        }
        for (&id, c) in sockets.iter_mut() {
            if let Some((options, server)) = tls_options.remove(&id) {
                c.tls_server = server;
                c.tls_verification = options.verification.clone();
                let result = if server {
                    super::tls::server(&options)
                } else {
                    super::tls::client(&options)
                };
                match result {
                    Ok(mut tls) => {
                        tls.set_buffer_limit(Some(65536));
                        c.tls = Some(tls);
                    }
                    Err(e) => {
                        c.failed = true;
                        error(&out, id, e);
                    }
                }
            }
            if c.failed {
                let _ = c.register(&poll, id);
                continue;
            }
            if c.end
                && c.writes.is_empty()
                && c.tls_written.is_none()
                && !c.connecting
                && c.tunnel.is_none()
                && (c.tls.is_none() || c.tls_permitted)
            {
                if let Some(tls) = c.tls.as_mut() {
                    tls.send_close_notify();
                    c.finishing = true;
                } else {
                    let _ = c.socket.shutdown(Shutdown::Write);
                    let _ = out.send((id, Event::Finished));
                }
                c.end = false;
            }
            if c.timeout != 0 && c.activity.elapsed() >= Duration::from_millis(c.timeout) {
                c.activity = Instant::now();
                let _ = out.send((id, Event::Timeout));
            }
            if c.tls.is_some()
                && !c.connecting
                && c.tunnel.is_none()
                && let Err(e) = tls_step(c, id, false, &out)
            {
                c.failed = true;
                error(&out, id, e);
            }
            if let Err(e) = c.register(&poll, id) {
                error(&out, id, e);
            }
        }
        if poll
            .poll(&mut events, Some(Duration::from_millis(20)))
            .is_err()
        {
            return;
        }
        for ready in events.iter() {
            let id = ready.token().0 as Id;
            if id == 0 {
                continue;
            }
            if let Some(d) = datagrams.get_mut(&id) {
                if ready.is_readable() && d.credit {
                    let mut bytes = vec![0; 65536];
                    match d.socket.recv_from(&mut bytes) {
                        Ok((n, peer)) => {
                            if d.inbound || d.peers.contains(&peer) {
                                bytes.truncate(n);
                                d.credit = false;
                                let _ = out.send((id, Event::Datagram(bytes, peer)));
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) => error(&out, id, e),
                    }
                }
                continue;
            }
            if let Some(l) = listeners.get_mut(&id) {
                match l.socket.accept() {
                    Ok((socket, peer)) => {
                        if let Ok(local) = socket.local_addr() {
                            accept_id += 1;
                            accepted.insert(accept_id, (id, socket));
                            let _ = out.send((id, Event::Accepted(accept_id, local, peer)));
                            let _ = poll.registry().deregister(&mut l.socket);
                            l.registered = false;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => error(&out, id, e),
                }
                continue;
            }
            let Some(c) = sockets.get_mut(&id) else {
                continue;
            };
            if c.failed {
                continue;
            }
            if c.connecting {
                match c.socket.take_error() {
                    Ok(Some(e)) => {
                        if let Some(address) = c.alternatives.pop_front() {
                            let _ = poll.registry().deregister(&mut c.socket);
                            c.registered = false;
                            match TcpStream::connect(address) {
                                Ok(socket) => c.socket = socket,
                                Err(e) => {
                                    c.failed = true;
                                    error(&out, id, e);
                                }
                            }
                        } else {
                            c.connecting = false;
                            c.failed = true;
                            error(&out, id, e);
                        }
                        continue;
                    }
                    Err(e) => {
                        c.failed = true;
                        error(&out, id, e);
                        continue;
                    }
                    Ok(None) => {}
                }
                if let (Ok(local), Ok(peer)) = (c.socket.local_addr(), c.socket.peer_addr()) {
                    c.connecting = false;
                    c.activity = Instant::now();
                    if c.tunnel.is_none() {
                        let _ = out.send((id, Event::Connected(local, peer)));
                    }
                } else {
                    continue;
                }
            }
            if c.tunnel.is_some() {
                match tunnel_step(c) {
                    Ok(Tunneled::Waiting) => {}
                    Ok(Tunneled::Open) => {
                        if let (Ok(local), Ok(peer)) = (c.socket.local_addr(), c.socket.peer_addr())
                        {
                            let _ = out.send((id, Event::Connected(local, peer)));
                        }
                    }
                    Ok(Tunneled::Refused(status, message)) => {
                        c.failed = true;
                        let _ = out.send((id, Event::TunnelRefused(status, message)));
                    }
                    Err(e) => {
                        c.failed = true;
                        error(&out, id, e);
                    }
                }
                continue;
            }
            if c.tls.is_some() {
                if let Err(e) = tls_step(c, id, ready.is_readable(), &out) {
                    c.failed = true;
                    error(&out, id, e);
                }
                continue;
            }
            if ready.is_writable() {
                // Fairness: at most one 64 KiB slice per ready socket per poll.
                if let Some((data, position)) = c.writes.front_mut() {
                    let end = (*position + 65536).min(data.len());
                    match c.socket.write(&data[*position..end]) {
                        Ok(n) => {
                            *position += n;
                            c.activity = Instant::now();
                            if *position == data.len() {
                                let n = data.len();
                                c.writes.pop_front();
                                let _ = out.send((id, Event::Written(n)));
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) => {
                            c.failed = true;
                            error(&out, id, e);
                        }
                    }
                }
            }
            if c.failed {
                continue;
            }
            if ready.is_readable() && c.credit && !c.ended {
                let mut data = vec![0; 65536];
                match c.socket.read(&mut data) {
                    Ok(0) => {
                        c.credit = false;
                        c.ended = true;
                        let _ = out.send((id, Event::End));
                    }
                    Ok(n) => {
                        data.truncate(n);
                        c.credit = false;
                        c.activity = Instant::now();
                        let _ = out.send((id, Event::Data(data)));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => {
                        c.failed = true;
                        error(&out, id, e);
                    }
                }
            }
        }
    }
}
fn udp_buffers(out: &Output, id: Id, socket: &UdpSocket) {
    let socket = socket2::SockRef::from(socket);
    if let (Ok(recv), Ok(send)) = (socket.recv_buffer_size(), socket.send_buffer_size()) {
        let _ = out.send((id, Event::DatagramBuffers(recv, send)));
    }
}
fn udp_option(socket: &UdpSocket, name: &str, value: &str, extra: &str) -> io::Result<()> {
    let sock = socket2::SockRef::from(socket);
    let integer = || {
        value
            .parse::<u32>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid socket option"))
    };
    let boolean = value == "true" || value == "1";
    let v6 = socket.local_addr()?.is_ipv6();
    let iface4 = || {
        if extra.is_empty() {
            Ok(std::net::Ipv4Addr::UNSPECIFIED)
        } else {
            extra.parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid multicast interface")
            })
        }
    };
    match name {
        "setBroadcast" => socket.set_broadcast(boolean),
        "setTTL" => socket.set_ttl(integer()?),
        "setMulticastTTL" => {
            if v6 {
                sock.set_multicast_hops_v6(integer()?)
            } else {
                socket.set_multicast_ttl_v4(integer()?)
            }
        }
        "setMulticastLoopback" => {
            if v6 {
                socket.set_multicast_loop_v6(boolean)
            } else {
                socket.set_multicast_loop_v4(boolean)
            }
        }
        "setRecvBufferSize" => sock.set_recv_buffer_size(integer()? as usize),
        "setSendBufferSize" => sock.set_send_buffer_size(integer()? as usize),
        "setMulticastInterface" => {
            if v6 {
                sock.set_multicast_if_v6(integer()?)
            } else {
                sock.set_multicast_if_v4(&value.parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid multicast interface")
                })?)
            }
        }
        "addMembership" | "dropMembership" => {
            let ip = value.parse::<IpAddr>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid multicast address")
            })?;
            match ip {
                IpAddr::V4(ip) => {
                    if name == "addMembership" {
                        socket.join_multicast_v4(&ip, &iface4()?)
                    } else {
                        socket.leave_multicast_v4(&ip, &iface4()?)
                    }
                }
                IpAddr::V6(ip) => {
                    let iface = extra.parse::<u32>().unwrap_or(0);
                    if name == "addMembership" {
                        socket.join_multicast_v6(&ip, iface)
                    } else {
                        socket.leave_multicast_v6(&ip, iface)
                    }
                }
            }
        }
        "disconnect" => sock.connect(&socket2::SockAddr::from(if v6 {
            SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0)
        })),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported datagram option",
        )),
    }
}

fn tls_step(c: &mut Connection, id: Id, readable: bool, out: &Output) -> io::Result<()> {
    let Some(tls) = c.tls.as_mut() else {
        return Ok(());
    };
    if readable && ((c.credit && c.tls_permitted) || tls.is_handshaking()) && !c.ended {
        match tls.read_tls(&mut c.socket) {
            Ok(0) => {
                if tls.is_handshaking() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "TLS peer closed during handshake",
                    ));
                }
                c.ended = true;
                // Drain any buffered plaintext before publishing EOF below.
            }
            Ok(_) => {
                let state = tls.process_new_packets().map_err(|error| {
                    let saved = c
                        .tls_verification
                        .lock()
                        .ok()
                        .and_then(|saved| saved.clone());
                    let code = saved
                        .as_deref()
                        .unwrap_or_else(|| super::tls::certificate_code(&error));
                    io::Error::other(format!("{code}: {error}"))
                })?;
                if state.peer_has_closed() {
                    c.ended = true;
                }
                c.activity = Instant::now();
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
    }
    if !tls.is_handshaking() && !c.secured {
        c.secured = true;
        let protocol = match tls.protocol_version() {
            Some(rustls::ProtocolVersion::TLSv1_3) => "TLSv1.3",
            _ => "TLSv1.2",
        }
        .to_owned();
        let alpn = tls.alpn_protocol().unwrap_or_default().to_vec();
        let cipher = tls
            .negotiated_cipher_suite()
            .map(|s| format!("{:?}", s.suite()))
            .unwrap_or_default();
        let cert = tls
            .peer_certificates()
            .and_then(|v| v.first())
            .map(|v| v.as_ref().to_vec())
            .unwrap_or_default();
        let verification = c
            .tls_verification
            .lock()
            .ok()
            .and_then(|error| error.clone());
        let authorized = verification.is_none() && (!c.tls_server || !cert.is_empty());
        let _ = out.send((
            id,
            Event::Secure(super::tls::SessionInfo {
                protocol,
                alpn,
                cipher,
                cert,
                authorized,
                authorization_error: verification,
            }),
        ));
    }
    if c.secured
        && c.tls_permitted
        && c.tls_written.is_none()
        && let Some((bytes, offset)) = c.writes.front_mut()
    {
        match tls.writer().write(&bytes[*offset..]) {
            Ok(n) => {
                *offset += n;
                if *offset == bytes.len() {
                    c.tls_written = Some(bytes.len());
                    c.writes.pop_front();
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
    }
    if tls.wants_write() {
        match tls.write_tls(&mut c.socket) {
            Ok(_) => {
                c.activity = Instant::now();
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
    }
    if !tls.wants_write() {
        if let Some(n) = c.tls_written.take() {
            let _ = out.send((id, Event::Written(n)));
        }
        if c.finishing {
            c.finishing = false;
            let _ = out.send((id, Event::Finished));
        }
    }
    if c.credit && c.secured && c.tls_permitted {
        let mut bytes = vec![0; 65536];
        match tls.reader().read(&mut bytes) {
            Ok(0) => {
                if c.ended {
                    c.credit = false;
                    let _ = out.send((id, Event::End));
                }
            }
            Ok(n) => {
                bytes.truncate(n);
                c.credit = false;
                let _ = out.send((id, Event::Data(bytes)));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// What a connection still being resolved has been asked to do once it is open.
#[derive(Default)]
struct Waiting {
    read: bool,
    writes: VecDeque<(Vec<u8>, usize)>,
    end: bool,
    timeout: u64,
    tunnel: Option<Tunnel>,
}
type Pending = HashMap<Id, Waiting>;
fn open(
    id: Id,
    addresses: Vec<SocketAddr>,
    sockets: &mut HashMap<Id, Connection>,
    pending: &mut Pending,
    poll: &Poll,
    out: &Output,
) {
    let mut addresses: VecDeque<_> = addresses.into();
    let mut last = io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no address for requested family",
    );
    while let Some(address) = addresses.pop_front() {
        match TcpStream::connect(address) {
            Ok(socket) => {
                let mut c = Connection::new(socket, true);
                c.alternatives = addresses;
                if let Some(waiting) = pending.remove(&id) {
                    c.credit = waiting.read;
                    c.writes = waiting.writes;
                    c.end = waiting.end;
                    c.timeout = waiting.timeout;
                    c.tunnel = waiting.tunnel;
                }
                if let Err(e) = c.register(poll, id) {
                    error(out, id, e);
                } else {
                    sockets.insert(id, c);
                }
                return;
            }
            Err(e) => last = e,
        }
    }
    pending.remove(&id);
    error(out, id, last);
}
