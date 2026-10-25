//! Frozen Node v22.21.1 observations (the Node 22.20 API target), plus native I/O acceptance.
//! No Node installation is required for these tests. Slow acceptance tests are explicitly ignored.
use std::time::{Duration, Instant};
use verve::{ListenPermission, NetworkRule, Permissions, Runtime};

#[path = "support/tls.rs"]
mod tls;

fn granted() -> Permissions {
    Permissions {
        outbound: vec![NetworkRule {
            host: None,
            port: None,
        }],
        listen: ListenPermission::Loopback,
        dns: true,
        child_process: false,
        ..Permissions::default()
    }
}
fn run(script: &str, permissions: Permissions) -> String {
    run_at(
        script,
        permissions,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/network-entry.cjs"),
    )
}
fn run_at(script: &str, permissions: Permissions, entry: std::path::PathBuf) -> String {
    let script = script.to_owned();
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            verve::set_thread_stack(16 * 1024 * 1024);
            run_inner(&script, permissions, &entry)
        })
        .unwrap()
        .join()
        .unwrap()
}
fn run_inner(script: &str, permissions: Permissions, entry: &std::path::Path) -> String {
    let mut rt = Runtime::new();
    rt.set_permissions(permissions);
    rt.eval(&tls::prelude()).unwrap();
    rt.load_module(entry, script).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        rt.run_network_until(Some(Instant::now() + Duration::from_millis(5)));
        rt.run_due();
        let transport = rt.network_diagnostics();
        assert!(
            transport.reactor_threads <= 1 && transport.dns_threads <= 2,
            "connections created extra network threads: {transport:?}"
        );
        // Read credit bounds native delivery independently of total bytes transferred. Allow
        // a single in-flight chunk after the last socket closes before this snapshot.
        assert!(
            transport.queued_read_bytes <= (transport.open_sockets + 1) * 64 * 1024,
            "unbounded network read queue: {transport:?}"
        );
        if rt
            .eval("globalThis.result !== undefined")
            .unwrap()
            .to_boolean()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "network test timed out: {:?}",
            rt.take_console()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    verve::print(&rt.eval("JSON.stringify(globalThis.result)").unwrap())
}
#[test]
fn default_permissions_deny_every_capability() {
    let permissions = Permissions::default();
    assert!(!permissions.allows_outbound("localhost", 80));
    assert!(!permissions.allows_listen("127.0.0.1".parse().unwrap()));
    assert!(!permissions.child_process);
}
#[test]
fn outbound_rules_match_host_and_port_together() {
    let p = Permissions {
        outbound: vec![NetworkRule {
            host: Some("example.org".into()),
            port: Some(443),
        }],
        ..Permissions::default()
    };
    assert!(p.allows_outbound("EXAMPLE.org", 443));
    assert!(!p.allows_outbound("example.org.attacker.invalid", 443));
    assert!(!p.allows_outbound("example.org", 80));
    assert!(!p.allows_listen("127.0.0.1".parse().unwrap()));
}
#[test]
fn loopback_listening_is_independent_of_outbound() {
    let p = Permissions {
        listen: ListenPermission::Loopback,
        ..Permissions::default()
    };
    assert!(p.allows_listen("::1".parse().unwrap()));
    assert!(p.allows_listen("127.1.2.3".parse().unwrap()));
    assert!(!p.allows_listen("0.0.0.0".parse().unwrap()));
    assert!(!p.allows_outbound("localhost", 80));
}
#[test]
fn denied_socket_retains_original_code_and_message() {
    assert_eq!(
        run(
            r#"const net=require('net');const s=net.connect(80,'127.0.0.1');s.on('error',e=>{globalThis.result=[e.code,e.message]});"#,
            Permissions::default()
        ),
        r#"["ECONNREFUSED","connect ECONNREFUSED: Verve does not let plugins open network connections with Node's modules: use requestUrl"]"#
    );
}
#[test]
fn sockets_are_duplex_and_echo_with_node_event_order() {
    assert_eq!(
        run(
            r#"
        const net=require('net'),{Duplex}=require('stream');const order=[];
        const server=net.createServer(s=>s.pipe(s));server.listen(0,'127.0.0.1',()=>{
            const s=net.connect(server.address().port,'127.0.0.1');order.push(s instanceof Duplex);
            s.on('connect',()=>order.push('connect'));s.on('ready',()=>{order.push('ready');s.end('hello')});
            s.setEncoding('utf8');s.on('data',s=>order.push(s));s.on('end',()=>order.push('end'));
            s.on('close',()=>{order.push('close');server.close(()=>globalThis.result=order)});
        });
    "#,
            granted()
        ),
        r#"[true,"connect","ready","hello","end","close"]"#
    );
}
#[test]
fn pause_stops_read_delivery_until_resume() {
    assert_eq!(
        run(
            r#"
        const net=require('net');let paused=false,stable=false,received=0;
        const server=net.createServer(s=>s.end(Buffer.alloc(512*1024)));
        server.listen(0,'127.0.0.1',()=>{
            const s=net.connect(server.address().port,'127.0.0.1');
            s.on('data',b=>{received+=b.length;if(!paused){paused=true;s.pause();const before=received;setTimeout(()=>{stable=received===before;s.resume()},20)}});
            s.on('close',()=>server.close(()=>globalThis.result=[stable,received===512*1024]));
        });
    "#,
            granted()
        ),
        "[true,true]"
    );
}
#[test]
fn tcp_socket_options_and_inactivity_timeout_are_chainable() {
    assert_eq!(
        run(
            r#"
        const net=require('net');const server=net.createServer(()=>{});
        server.listen(0,'127.0.0.1',()=>{
            const s=net.connect(server.address().port,'127.0.0.1');const chain=s.setNoDelay(true)===s&&s.setKeepAlive(true,1000)===s&&s.setTimeout(20)===s;
            s.on('timeout',()=>{s.destroy();server.close(()=>globalThis.result=[chain,s.timeout])});
        });
    "#,
            granted()
        ),
        "[true,20]"
    );
}
#[test]
fn dns_lookup_promises_uses_system_pool() {
    assert_eq!(
        run(
            r#"require('dns').promises.lookup('127.0.0.1',{all:true}).then(v=>globalThis.result=v)"#,
            granted()
        ),
        r#"[{"address":"127.0.0.1","family":4}]"#
    );
}
#[test]
fn dns_record_shapes_and_ttl_match_node() {
    assert_eq!(
        run(include_str!("fixtures/network/dns-records.cjs"), granted()),
        r#"[[{"address":"127.0.0.1","ttl":300}],["::1"],[["one","two"]],[{"name":"target.test","port":8443,"priority":1,"weight":2}]]"#
    );
}
#[test]
fn ipv6_tcp_loopback_is_duplex() {
    assert_eq!(
        run(
            r#"
        const net=require('net');const server=net.createServer(s=>s.pipe(s));
        server.on('error',e=>{if(e.code==='EAFNOSUPPORT'||e.code==='EADDRNOTAVAIL')globalThis.result='IPv6 unavailable';else throw e});
        server.listen(0,'::1',()=>{const s=net.connect({host:'::1',port:server.address().port,family:6});let body='';s.setEncoding('utf8');s.on('data',b=>body+=b);s.on('connect',()=>s.end('ipv6'));s.on('close',()=>server.close(()=>globalThis.result=body))});
    "#,
            granted()
        ),
        r#""ipv6""#
    );
}
fn node_peer() -> std::process::Command {
    let mut command = std::process::Command::new(
        std::env::var_os("VERVE_NETWORK_NODE").unwrap_or_else(|| "node".into()),
    );
    command.arg(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/network/peer.cjs"),
    );
    command
}
#[test]
#[ignore = "optional live Node cross-peer; frozen tests do not require Node"]
fn runtime_client_connects_to_node_server() {
    use std::io::BufRead;
    let mut child = node_peer()
        .arg("server")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line.trim().parse::<u16>().unwrap();
    let observed = run(
        &format!(
            r#"
        const order=[],s=require('net').connect({port},'127.0.0.1');s.setEncoding('utf8');
        s.on('connect',()=>order.push('connect'));s.on('ready',()=>{{order.push('ready');s.end('from-runtime')}});
        s.on('data',b=>order.push(b));s.on('end',()=>order.push('end'));s.on('close',()=>{{order.push('close');globalThis.result=order}});
    "#
        ),
        granted(),
    );
    assert!(child.wait().unwrap().success());
    assert_eq!(
        observed,
        r#"["connect","ready","from-runtime","end","close"]"#
    );
}
#[test]
#[ignore = "optional live Node cross-peer; frozen tests do not require Node"]
fn node_client_connects_to_runtime_server() {
    std::thread::Builder::new().stack_size(16*1024*1024).spawn(|| {
        verve::set_thread_stack(16*1024*1024);
        let mut rt=Runtime::new();rt.set_permissions(granted());
        rt.load_module(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/network-entry.cjs"), r#"
            const net=require('net'),order=[];const server=net.createServer(s=>{
                order.push('connection');s.setEncoding('utf8');s.on('data',b=>order.push(b));s.pipe(s);
                s.on('end',()=>order.push('end'));s.on('close',()=>{order.push('close');server.close(()=>globalThis.result=order)});
            });server.listen(0,'127.0.0.1',()=>globalThis.port=server.address().port);
        "#).unwrap();
        let deadline=Instant::now()+Duration::from_secs(30);let mut child=None;
        while !rt.eval("globalThis.result!==undefined").unwrap().to_boolean() {
            rt.run_due();
            if child.is_none()
                && let Some(port) = rt.eval("globalThis.port").unwrap().as_number()
            {
                child = Some(node_peer().arg("client").arg((port as u16).to_string()).stdout(std::process::Stdio::piped()).spawn().unwrap());
            }
            assert!(Instant::now()<deadline,"Node cross-peer timed out");std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(verve::print(&rt.eval("JSON.stringify(result)").unwrap()),r#"["connection","from-node","end","close"]"#);
        drop(rt);
        let output=child.unwrap().wait_with_output().unwrap();assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(),r#"["connect","ready","from-node","end","close"]"#);
    }).unwrap().join().unwrap();
}

#[test]
fn replacing_permissions_closes_listeners_and_completes_pending_dns() {
    let mut rt = Runtime::new();
    rt.set_permissions(granted());
    rt.load_module(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/network-entry.cjs"),
        r#"
        globalThis.closed=false;globalThis.dnsCode='';
        globalThis.server=require('net').createServer();server.on('close',()=>closed=true);
        server.listen(0,'127.0.0.1');require('dns').lookup('127.0.0.1',e=>dnsCode=e.code);
    "#,
    )
    .unwrap();
    rt.set_permissions(Permissions::default());
    let deadline = Instant::now() + Duration::from_secs(2);
    while !rt.eval("closed&&dnsCode!==''").unwrap().to_boolean() {
        rt.run_due();
        assert!(
            Instant::now() < deadline,
            "policy revocation callbacks were lost"
        );
    }
    assert_eq!(
        verve::print(
            &rt.eval("JSON.stringify([closed,dnsCode,server.listening,server.address()])")
                .unwrap()
        ),
        r#"[true,"EACCES",false,null]"#
    );
}
#[test]
fn expired_network_deadline_returns_without_waiting() {
    let mut rt = Runtime::new();
    rt.set_permissions(granted());
    rt.load_module(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/network-entry.cjs"),
        "require('dns').lookup('verve-does-not-exist.invalid',()=>{})",
    )
    .unwrap();
    let start = Instant::now();
    assert_eq!(rt.run_network_until(Some(start)), 0);
    assert!(start.elapsed() < Duration::from_millis(100));
    let start = Instant::now();
    drop(rt);
    assert!(start.elapsed() < Duration::from_millis(100));
}
#[test]
fn tls_client_server_ca_alpn_identity_and_duplex() {
    assert_eq!(
        run(
            r#"
        const tls=require('tls'),fs=require('fs'),{Duplex}=require('stream');
        const cert=TLS_CERT;
        const key=TLS_KEY;
        let checked=false;const server=tls.createServer({cert,key,ALPNProtocols:['http/1.1']},s=>s.end('secure'));
        server.listen(0,'127.0.0.1',()=>{
            const s=tls.connect({host:'127.0.0.1',port:server.address().port,servername:'localhost',ca:cert,ALPNProtocols:['http/1.1'],checkServerIdentity:(name,cert)=>{checked=name==='localhost'||name==='127.0.0.1';if(!cert.raw)throw Error('no certificate')}},()=>{});
            let body='';s.setEncoding('utf8');s.on('data',b=>body+=b);
            s.on('end',()=>{globalThis.result=[s instanceof Duplex,s.authorized,s.alpnProtocol,body,checked,s.getPeerCertificate().subject.CN];s.end();server.close()});
        });
    "#,
            granted()
        ),
        r#"[true,true,"http/1.1","secure",true,"localhost"]"#
    );
}

#[test]
fn tls_untrusted_peer_reports_authorization_without_releasing_early_data() {
    assert_eq!(
        run(
            r#"
        const tls=require('tls'),fs=require('fs');
        const cert=TLS_CERT;
        const key=TLS_KEY;
        let checked=false;const server=tls.createServer({cert,key},s=>s.end('untrusted'));
        server.listen(0,'127.0.0.1',()=>{
            const s=tls.connect({host:'127.0.0.1',port:server.address().port,servername:'localhost',rejectUnauthorized:false,checkServerIdentity:()=>{checked=true}});
            let body='';s.setEncoding('utf8');s.on('data',b=>body+=b);
            s.on('end',()=>{globalThis.result=[s.authorized,s.authorizationError,checked,body];s.end();server.close()});
        });
    "#,
            granted()
        ),
        r#"[false,"DEPTH_ZERO_SELF_SIGNED_CERT",false,"untrusted"]"#
    );
}
#[test]
fn tls_identity_rejection_never_exposes_application_bytes() {
    assert_eq!(
        run(
            r#"
        const tls=require('tls'),fs=require('fs');
        const cert=TLS_CERT;
        const key=TLS_KEY;
        const server=tls.createServer({cert,key},s=>{s.on('error',()=>{});s.end('private')});
        server.on('tlsClientError',()=>{});let code='',bytes=0;
        server.listen(0,'127.0.0.1',()=>{
            const s=tls.connect({host:'127.0.0.1',port:server.address().port,ca:cert,checkServerIdentity:()=>Object.assign(Error('identity denied'),{code:'CUSTOM_IDENTITY'})});
            s.on('data',b=>bytes+=b.length);s.on('error',e=>code=e.code);
            s.on('close',()=>server.close(()=>globalThis.result=[code,bytes]));
        });
    "#,
            granted()
        ),
        r#"["CUSTOM_IDENTITY",0]"#
    );
}
#[test]
fn https_agent_uses_explicit_ca_and_real_server_type() {
    assert_eq!(
        run(
            r#"
        const https=require('https'),fs=require('fs');
        const cert=TLS_CERT;
        const key=TLS_KEY;
        const agent=new https.Agent({keepAlive:true,ca:cert});
        const server=new https.Server({cert,key},(req,res)=>res.end('https'));
        server.listen(0,'127.0.0.1',()=>https.get({hostname:'127.0.0.1',port:server.address().port,agent},res=>{
            const authorized=res.socket.authorized;let body='';res.setEncoding('utf8');res.on('data',b=>body+=b);
            res.on('end',()=>{globalThis.result=[server instanceof https.Server,res.statusCode,authorized,body];agent.destroy();server.close()});
        }));
    "#,
            granted()
        ),
        r#"[true,200,true,"https"]"#
    );
}

#[test]
fn http_chunked_raw_headers_cookies_and_no_redirect() {
    assert_eq!(
        run(
            r#"
        const http=require('http');const server=http.createServer((req,res)=>{
            res.writeHead(302,{Location:'/not-followed','Set-Cookie':['a=1','b=2'],'X-Mixed':['one','two']});res.write('hel');res.end('lo');
        });server.listen(0,'127.0.0.1',()=>http.get({host:'127.0.0.1',port:server.address().port,agent:false},res=>{
            let body='';res.setEncoding('utf8');res.on('data',b=>body+=b);res.on('end',()=>{globalThis.result=[res.statusCode,res.headers.location,res.headers['set-cookie'],res.headers['x-mixed'],res.rawHeaders.includes('X-Mixed'),body];server.close()});
        }));
    "#,
            granted()
        ),
        r#"[302,"/not-followed",["a=1","b=2"],"one, two",true,"hello"]"#
    );
}
#[test]
fn http_agent_reuses_consumed_keepalive_socket() {
    assert_eq!(
        run(
            r#"
        const http=require('http');const agent=new http.Agent({keepAlive:true});let connections=0;
        const server=http.createServer((req,res)=>res.end('ok'));server.on('connection',()=>connections++);
        server.listen(0,'127.0.0.1',()=>{function get(n){http.get({host:'127.0.0.1',port:server.address().port,agent},res=>{res.resume();res.on('end',()=>setImmediate(()=>{if(n===1)get(2);else{globalThis.result=[connections,res.req.reusedSocket];agent.destroy();server.close()}}))})}get(1)});
    "#,
            granted()
        ),
        "[1,true]"
    );
}

#[test]
fn udp_send_receive_slice_and_connected_peer() {
    assert_eq!(
        run(
            r#"
        const dgram=require('dgram'),server=dgram.createSocket('udp4');
        server.on('message',(b,peer)=>server.send(b,peer.port,peer.address));
        server.bind(0,'127.0.0.1',()=>{
            const client=dgram.createSocket('udp4');
            client.connect(server.address().port,'127.0.0.1',()=>client.send(Buffer.from('xhello!'),1,5));
            client.on('message',(b,peer)=>{globalThis.result=[b.toString(),peer.family,peer.size,client.remoteAddress().address];client.close();server.close()});
        });
    "#,
            granted()
        ),
        r#"["hello","IPv4",5,"127.0.0.1"]"#
    );
}
#[test]
fn forged_udp_peer_metadata_cannot_bypass_outbound_rules() {
    let permissions = Permissions {
        outbound: vec![NetworkRule {
            host: Some("allowed.invalid".into()),
            port: Some(443),
        }],
        ..Permissions::default()
    };
    assert_eq!(
        run(
            r#"
        const s=require('dgram').createSocket('udp4');
        // Hidden JavaScript properties are metadata, not a capability token.
        s.__udpPeer={address:'127.0.0.1',port:9};
        s.send('denied',e=>{globalThis.result=e.code;s.close()});
    "#,
            permissions
        ),
        r#""EACCES""#
    );
}
#[test]
fn udp_denial_does_not_start_listening() {
    assert_eq!(
        run(
            r#"
        const s=require('dgram').createSocket('udp4');
        s.on('error',e=>globalThis.result=e.code);s.bind(0,'127.0.0.1');
    "#,
            Permissions::default()
        ),
        r#""EACCES""#
    );
}
#[test]
fn http_close_all_connections_closes_active_transport() {
    assert_eq!(
        run(
            r#"
        const http=require('http');const server=http.createServer((req,res)=>{res.write('open');setImmediate(()=>server.closeAllConnections())});
        server.listen(0,'127.0.0.1',()=>{
            const req=http.get({host:'127.0.0.1',port:server.address().port,agent:false},res=>{
                res.on('error',()=>{});res.resume();res.on('close',()=>{globalThis.result=true;server.close()});
            });req.on('error',()=>{globalThis.result=true;server.close()});
        });
    "#,
            granted()
        ),
        "true"
    );
}
#[test]
fn http_upgrade_completes_messages_without_aborting_them_on_close() {
    assert_eq!(
        run(
            r#"
        const http=require('http');const complete=[];let aborted=0;
        const server=http.createServer();
        server.on('upgrade',(req,socket)=>{
            complete.push(req.complete);req.on('aborted',()=>aborted++);
            socket.end('HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: test\r\n\r\n');
        });
        server.listen(0,'127.0.0.1',()=>{
            const req=http.request({host:'127.0.0.1',port:server.address().port,headers:{Connection:'Upgrade',Upgrade:'test'}});
            req.on('upgrade',(res,socket)=>{
                complete.push(res.complete);res.on('aborted',()=>aborted++);
                socket.resume();socket.on('end',()=>socket.end());
                socket.on('close',()=>server.close(()=>setImmediate(()=>globalThis.result=[...complete,aborted])));
            });req.end();
        });
    "#,
            granted()
        ),
        "[true,true,0]"
    );
}

#[test]
fn http_server_close_finishes_active_keepalive_responses() {
    assert_eq!(
        run(
            r#"
        const http=require('http');let closed=false,ended=false,body='';
        function done(){if(closed&&ended)globalThis.result=[body,closed]}
        const server=http.createServer((req,res)=>{
            server.close(()=>{closed=true;done()});setTimeout(()=>res.end('complete'),1);
        });
        server.listen(0,'127.0.0.1',()=>http.get({host:'127.0.0.1',port:server.address().port},res=>{
            res.on('data',b=>body+=b);res.on('end',()=>{ended=true;done()});
        }));
    "#,
            granted()
        ),
        r#"["complete",true]"#
    );
}

#[test]
#[ignore = "prepare offline npm fixture and set VERVE_NETWORK_NPM; central integration"]
fn real_ws_axios_node_fetch_and_express_packages() {
    let root = std::path::PathBuf::from(
        std::env::var_os("VERVE_NETWORK_NPM")
            .expect("Run tests/fixtures/network/prepare.cjs with an absolute scratch destination"),
    );
    let entry = root.join("npm-smoke.cjs");
    let script = std::fs::read_to_string(&entry).unwrap();
    // Node 22.21.1, using the exact same source and the vendored lockfile, not fake packages.
    assert_eq!(
        run_at(&script, granted(), entry),
        r#"["npm-ws","1,2,3",1000,200,42,200,42,302]"#
    );
}

#[test]
#[ignore = "100 MB acceptance; run during central integration"]
fn loopback_100mb_obeys_backpressure() {
    assert_eq!(
        run(
            r#"
        const net=require('net');let received=0,drains=0,written=0,peak=0;const size=100*1024*1024;
        const server=net.createServer(s=>{s.on('data',b=>received+=b.length);s.on('end',()=>s.end())});
        server.listen(0,'127.0.0.1',()=>{const s=net.connect(server.address().port,'127.0.0.1');
            const chunk=Buffer.alloc(64*1024);function pump(){while(written<size){written+=chunk.length;const ready=s.write(chunk);peak=Math.max(peak,s.writableLength);if(!ready){drains++;return}}s.end()}
            s.on('drain',pump);s.on('connect',pump);s.resume();s.on('close',()=>server.close(()=>globalThis.result=[received===size,drains>0,s.writableLength===0&&peak<=64*1024]));
        });
    "#,
            granted()
        ),
        "[true,true,true]"
    );
}
#[test]
#[ignore = "1000 connection acceptance; run during central integration"]
fn thousand_connections_share_one_reactor() {
    assert_eq!(
        run(
            r#"
        const net=require('net');let started=0,connected=0,accepted=0,closed=0;const sockets=[];
        function finish(){if(connected===1000&&accepted===1000){for(const t of sockets)t.destroy();server.close()}}
        const server=net.createServer(s=>{accepted++;s.on('error',()=>{});finish()});
        server.listen(0,'127.0.0.1',2048,()=>{
            function open(){started++;const s=net.connect(server.address().port,'127.0.0.1');sockets.push(s);
                s.on('connect',()=>{connected++;if(started<1000)open();finish()});
                s.on('error',e=>{throw e});s.on('close',()=>{if(++closed===1000)globalThis.result=connected});
            }
            // Bound pending SYNs, not established connections: all 1000 remain live until accepted.
            for(let i=0;i<10;i++)open();
        });
    "#,
            granted()
        ),
        "1000"
    );
}

/// With a waker, a host sleeps until the network has something instead of looking every
/// millisecond: `next_due` stops asking for that poll, and the reactor wakes the host.
#[test]
fn a_waker_replaces_the_millisecond_poll() {
    use std::sync::{Arc, Condvar, Mutex};
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            verve::set_thread_stack(16 * 1024 * 1024);
            let mut rt = Runtime::new();
            rt.set_permissions(granted());
            let woken = Arc::new((Mutex::new(0usize), Condvar::new()));
            let signal = woken.clone();
            rt.set_waker(Some(Arc::new(move || {
                let (count, ready) = &*signal;
                *count.lock().unwrap() += 1;
                ready.notify_all();
            })));
            rt.load_module(
                &std::env::temp_dir().join("verve-waker.js"),
                "const net = require('net');
                 const server = net.createServer((s) => s.end('woken'));
                 server.listen(0, '127.0.0.1', () => {
                     const c = net.connect(server.address().port, '127.0.0.1');
                     let got = '';
                     c.on('data', (d) => got += d);
                     c.on('end', () => { globalThis.result = got; server.close(); });
                 });",
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut slept = 0;
            let (count, ready) = &*woken;
            loop {
                // Read before looking, so a wake while looking is not lost.
                let before = *count.lock().unwrap();
                rt.run_due();
                if rt
                    .eval("globalThis.result !== undefined")
                    .unwrap()
                    .to_boolean()
                {
                    break;
                }
                // No timer is set, so nothing is due: the network wakes the host instead.
                assert_eq!(rt.next_due(), None);
                let (after, timeout) = ready
                    .wait_timeout_while(count.lock().unwrap(), Duration::from_secs(10), |n| {
                        *n == before
                    })
                    .unwrap();
                drop(after);
                assert!(!timeout.timed_out(), "the network never woke the host");
                slept += 1;
                assert!(Instant::now() < deadline, "never finished");
            }
            assert!(slept >= 1);
            assert_eq!(
                verve::print(&rt.eval("globalThis.result").unwrap()),
                "woken"
            );
            // Without one, the old contract: an open server asks to be looked at shortly.
            let mut polled = Runtime::new();
            polled.set_permissions(granted());
            let _ = polled.load_module(
                &std::env::temp_dir().join("verve-no-waker.js"),
                "require('net').createServer().listen(0, '127.0.0.1');",
            );
            assert!(polled.next_due().is_some());
        })
        .unwrap()
        .join()
        .unwrap();
}
