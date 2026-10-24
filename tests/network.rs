//! Frozen Node v22.21.1 observations (the Node 22.20 API target), plus native I/O acceptance.
//! No Node installation is required for these tests. Slow acceptance tests are explicitly ignored.
use runtime_x::{ListenPermission, NetworkRule, Permissions, RuntimeX};
use std::time::{Duration, Instant};

fn granted() -> Permissions {
    Permissions {
        outbound: vec![NetworkRule {
            host: None,
            port: None,
        }],
        listen: ListenPermission::Loopback,
        dns: true,
        child_process: false,
    }
}
fn run(script: &str, permissions: Permissions) -> String {
    let mut rt = RuntimeX::new();
    rt.set_permissions(permissions);
    rt.load_module(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/network-entry.cjs"),
        script,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        rt.run_network_until(Some(Instant::now() + Duration::from_millis(5)));
        rt.run_due();
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
    runtime_x::print(&rt.eval("JSON.stringify(globalThis.result)").unwrap())
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
        r#"["ECONNREFUSED","connect ECONNREFUSED: Su does not let plugins open network connections with Node's modules: use requestUrl"]"#
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
fn expired_network_deadline_returns_without_waiting() {
    let mut rt = RuntimeX::new();
    rt.set_permissions(granted());
    rt.eval("require('dns').lookup('runtime-x-does-not-exist.invalid',()=>{})")
        .unwrap();
    let start = Instant::now();
    assert_eq!(rt.run_network_until(Some(start)), 0);
    assert!(start.elapsed() < Duration::from_millis(100));
    let start = Instant::now();
    drop(rt);
    assert!(start.elapsed() < Duration::from_millis(100));
}
#[test]
#[ignore = "100 MB acceptance; run during central integration"]
fn loopback_100mb_obeys_backpressure() {
    assert_eq!(
        run(
            r#"
        const net=require('net');let received=0,drains=0,written=0;const size=100*1024*1024;
        const server=net.createServer(s=>{s.on('data',b=>received+=b.length);s.on('end',()=>s.end())});
        server.listen(0,'127.0.0.1',()=>{const s=net.connect(server.address().port,'127.0.0.1');
            const chunk=Buffer.alloc(64*1024);function pump(){while(written<size){written+=chunk.length;if(!s.write(chunk)){drains++;return}}s.end()}
            s.on('drain',pump);s.on('connect',pump);s.resume();s.on('close',()=>server.close(()=>globalThis.result=[received===size,drains>0,s.writableLength===0]));
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
        const net=require('net');let connected=0,closed=0;const sockets=[];
        const server=net.createServer(s=>s.on('error',()=>{}));
        server.listen(0,'127.0.0.1',()=>{for(let i=0;i<1000;i++){const s=net.connect(server.address().port,'127.0.0.1');sockets.push(s);
            s.on('connect',()=>{if(++connected===1000){for(const t of sockets)t.destroy();server.close()}});
            s.on('close',()=>{if(++closed===1000)globalThis.result=connected});}});
    "#,
            granted()
        ),
        "1000"
    );
}
