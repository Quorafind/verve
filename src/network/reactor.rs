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
    Listen(Id, SocketAddr),
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
    End,
    Closed,
    Timeout,
    Secure(String, Vec<u8>, String, Vec<u8>),
    Dns(Result<super::dns::Answer, String>),
    Datagram(Vec<u8>, SocketAddr),
    DatagramSent(Id, Result<usize, String>),
    DatagramConnected(SocketAddr, SocketAddr),
    Error(String, String),
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
}
impl Output {
    fn send(&self, event: (Id, Event)) -> Result<(), mpsc::SendError<(Id, Event)>> {
        let n = match &event.1 {
            Event::Data(bytes) | Event::Datagram(bytes, _) => bytes.len(),
            _ => 0,
        };
        self.counters
            .queued_read_bytes
            .fetch_add(n, Ordering::Relaxed);
        let result = self.tx.send(event);
        if result.is_err() {
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
    pub fn new() -> io::Result<Self> {
        let poll = Poll::new()?;
        let wake = Arc::new(Waker::new(poll.registry(), Token(0))?);
        let (tx, commands) = mpsc::channel();
        let (out, events) = mpsc::channel();
        let counters = Arc::new(Counters::default());
        let out = Output {
            tx: out,
            counters: counters.clone(),
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
                .name(format!("runtime-x-connect-dns-{n}"))
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
            .name("runtime-x-network".into())
            .spawn(move || {
                let counters = out.counters.clone();
                let _count = ThreadCount::new(&counters.reactor_threads);
                run(poll, commands, out, dns_tx);
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
    failed: bool,
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
            failed: false,
        }
    }
    fn interest(&self) -> Option<Interest> {
        if self.failed {
            return None;
        }
        match (
            (self.credit && !self.ended) || self.tls.as_ref().is_some_and(|t| t.is_handshaking()),
            self.connecting
                || !self.writes.is_empty()
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
    let _ = out.send((id, Event::Error(code(&e).into(), e.to_string())));
}
fn run(
    mut poll: Poll,
    commands: mpsc::Receiver<Command>,
    out: Output,
    dns: mpsc::SyncSender<(Id, super::dns::Query)>,
) {
    let mut sockets: HashMap<Id, Connection> = HashMap::new();
    let mut tls_options = HashMap::new();
    let mut datagrams: HashMap<Id, Datagram> = HashMap::new();
    let mut udp_jobs: HashMap<Id, UdpJob> = HashMap::new();
    let mut listeners: HashMap<Id, Listener> = HashMap::new();
    let mut pending = HashMap::new();
    let mut accepted: HashMap<Id, TcpStream> = HashMap::new();
    let mut accept_id = 1_u64 << 52;
    let mut events = Events::with_capacity(1024);
    loop {
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
                Command::TlsClient(id, options) => {
                    tls_options.insert(id, (options, false));
                }
                Command::TlsServer(id, options) => {
                    tls_options.insert(id, (options, true));
                }
                Command::UdpBind(id, address, inbound) => match UdpSocket::bind(address) {
                    Ok(socket) => {
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
                },
                Command::UdpSend(id, request, host, port, bytes) => {
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
                        if let Err(e) = udp_option(&d.socket, &name, &value, &extra) {
                            error(&out, id, e);
                        }
                    }
                }
                Command::Stop => return,
                Command::Connect(id, host, port, family) => {
                    pending.insert(id, (false, VecDeque::new(), false, 0));
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
                Command::Listen(id, address) => {
                    match TcpListener::bind(address).and_then(|mut socket| {
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
                    if let Some(l) = listeners.get_mut(&id) {
                        if !l.registered
                            && poll
                                .registry()
                                .register(&mut l.socket, Token(id as usize), Interest::READABLE)
                                .is_ok()
                        {
                            l.registered = true;
                        }
                    }
                }
                Command::Adopt(ticket, id) => {
                    if let Some(socket) = accepted.remove(&ticket) {
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
                        p.0 = enabled;
                    }
                }
                Command::Write(id, data) => {
                    if let Some(c) = sockets.get_mut(&id) {
                        c.writes.push_back((data, 0));
                    } else if let Some(p) = pending.get_mut(&id) {
                        p.1.push_back((data, 0));
                    }
                }
                Command::End(id) => {
                    if let Some(c) = sockets.get_mut(&id) {
                        c.end = true;
                    }
                    if let Some(p) = pending.get_mut(&id) {
                        p.2 = true;
                    }
                }
                Command::NoDelay(id, enabled) => {
                    if let Some(c) = sockets.get(&id) {
                        if let Err(e) = c.socket.set_nodelay(enabled) {
                            error(&out, id, e);
                        }
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
                        p.3 = ms;
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
            if c.end && c.writes.is_empty() && c.tls_written.is_none() && !c.connecting {
                if let Some(tls) = c.tls.as_mut() {
                    tls.send_close_notify();
                } else {
                    let _ = c.socket.shutdown(Shutdown::Write);
                }
                c.end = false;
            }
            if c.timeout != 0 && c.activity.elapsed() >= Duration::from_millis(c.timeout) {
                c.activity = Instant::now();
                let _ = out.send((id, Event::Timeout));
            }
            if c.tls.is_some() && !c.connecting {
                if let Err(e) = tls_step(c, id, false, &out) {
                    c.failed = true;
                    error(&out, id, e);
                }
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
                            accepted.insert(accept_id, socket);
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
                                Err(e) => error(&out, id, e),
                            }
                        } else {
                            c.connecting = false;
                            error(&out, id, e);
                        }
                        continue;
                    }
                    Err(e) => {
                        error(&out, id, e);
                        continue;
                    }
                    Ok(None) => {}
                }
                if let (Ok(local), Ok(peer)) = (c.socket.local_addr(), c.socket.peer_addr()) {
                    c.connecting = false;
                    c.activity = Instant::now();
                    let _ = out.send((id, Event::Connected(local, peer)));
                } else {
                    continue;
                }
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
                        Err(e) => error(&out, id, e),
                    }
                }
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
                    Err(e) => error(&out, id, e),
                }
            }
        }
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
    if readable && (c.credit || tls.is_handshaking()) && !c.ended {
        match tls.read_tls(&mut c.socket) {
            Ok(0) => {
                c.ended = true;
                let _ = out.send((id, Event::End));
            }
            Ok(_) => {
                let state = tls.process_new_packets().map_err(io::Error::other)?;
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
        let _ = out.send((id, Event::Secure(protocol, alpn, cipher, cert)));
    }
    if c.secured && c.tls_written.is_none() {
        if let Some((bytes, offset)) = c.writes.front_mut() {
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
    }
    if c.credit && c.secured {
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

type Pending = HashMap<Id, (bool, VecDeque<(Vec<u8>, usize)>, bool, u64)>;
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
                if let Some((read, writes, end, timeout)) = pending.remove(&id) {
                    c.credit = read;
                    c.writes = writes;
                    c.end = end;
                    c.timeout = timeout;
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
