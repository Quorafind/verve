//! One mio poller per realm. Read credits bound queued plaintext to one chunk per socket;
//! listeners likewise accept only after the preceding accept has reached JavaScript.
use mio::{
    Events, Interest, Poll, Token, Waker,
    net::{TcpListener, TcpStream},
};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr},
    sync::{Arc, Mutex, mpsc},
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
    Dns(Result<super::dns::Answer, String>),
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
pub(crate) struct Reactor {
    pub events: mpsc::Receiver<(Id, Event)>,
    pub sender: Sender,
}
impl Reactor {
    pub fn new() -> io::Result<Self> {
        let poll = Poll::new()?;
        let wake = Arc::new(Waker::new(poll.registry(), Token(0))?);
        let (tx, commands) = mpsc::channel();
        let (out, events) = mpsc::channel();
        let sender = Sender { tx, wake };
        let resolver = sender.clone();
        let (dns_tx, dns_rx) = mpsc::sync_channel::<(Id, super::dns::Query)>(1024);
        let dns_rx = Arc::new(Mutex::new(dns_rx));
        for n in 0..2 {
            let rx = dns_rx.clone();
            let sender = resolver.clone();
            thread::Builder::new()
                .name(format!("runtime-x-connect-dns-{n}"))
                .spawn(move || {
                    loop {
                        let job = match rx.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => break,
                        };
                        let Ok((id, query)) = job else {
                            break;
                        };
                        sender.send(Command::DnsResult(id, super::dns::resolve(query)));
                    }
                })?;
        }
        thread::Builder::new()
            .name("runtime-x-network".into())
            .spawn(move || {
                run(poll, commands, out, dns_tx);
            })?;
        Ok(Self { events, sender })
    }
    pub fn send(&self, command: Command) {
        self.sender.send(command);
    }
}
impl Drop for Reactor {
    fn drop(&mut self) {
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
        }
    }
    fn interest(&self) -> Option<Interest> {
        match (
            self.credit && !self.ended,
            self.connecting || !self.writes.is_empty(),
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
fn error(out: &mpsc::Sender<(Id, Event)>, id: Id, e: io::Error) {
    let _ = out.send((id, Event::Error(code(&e).into(), e.to_string())));
}
fn run(
    mut poll: Poll,
    commands: mpsc::Receiver<Command>,
    out: mpsc::Sender<(Id, Event)>,
    dns: mpsc::SyncSender<(Id, super::dns::Query)>,
) {
    let mut sockets: HashMap<Id, Connection> = HashMap::new();
    let mut listeners: HashMap<Id, Listener> = HashMap::new();
    let mut pending = HashMap::new();
    let mut accepted: HashMap<Id, TcpStream> = HashMap::new();
    let mut accept_id = 1_u64 << 52;
    let mut events = Events::with_capacity(1024);
    loop {
        for _ in 0..4096 {
            let command = match commands.try_recv() {
                Ok(c) => c,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            };
            match command {
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
                    pending.remove(&id);
                    accepted.remove(&id);
                    let _ = out.send((id, Event::Closed));
                }
            }
        }
        for (&id, c) in sockets.iter_mut() {
            if c.end && c.writes.is_empty() && !c.connecting {
                let _ = c.socket.shutdown(Shutdown::Write);
                c.end = false;
            }
            if c.timeout != 0 && c.activity.elapsed() >= Duration::from_millis(c.timeout) {
                c.activity = Instant::now();
                let _ = out.send((id, Event::Timeout));
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
type Pending = HashMap<Id, (bool, VecDeque<(Vec<u8>, usize)>, bool, u64)>;
fn open(
    id: Id,
    addresses: Vec<SocketAddr>,
    sockets: &mut HashMap<Id, Connection>,
    pending: &mut Pending,
    poll: &Poll,
    out: &mpsc::Sender<(Id, Event)>,
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
