//! `verve::npm` against a registry served from the test: packuments and tarballs made here, so
//! every rule is checked with no network. The `#[ignore]`d tests at the end reach the real
//! registry and are run by hand.
#![cfg(feature = "npm")]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use boa_engine::builtins::promise::PromiseState;
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};
use sha2::Digest;
use verve::npm::{InstallError, InstallOptions, Platform, Url, install, pack};

// ---------------------------------------------------------------------------------------------
// A temporary folder, removed when dropped.

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "verve-npm-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------------------------
// Tarballs.

/// A package tarball as `npm pack` makes one: every file under `package/`.
fn tarball(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(if path.starts_with("bin/") {
            0o755
        } else {
            0o644
        });
        header.set_cksum();
        builder
            .append_data(&mut header, format!("package/{path}"), data.as_slice())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// A tarball with entry names written as given, which `tar::Builder` would refuse.
fn raw_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (name, data) in entries {
        let mut header = tar::Header::new_old();
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn sri(bytes: &[u8]) -> String {
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(bytes))
    )
}

fn sha1_hex(bytes: &[u8]) -> String {
    sha1::Sha1::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The registry.

/// A package's dist-tags and version documents.
type Published = (BTreeMap<String, String>, BTreeMap<String, Value>);

#[derive(Default)]
struct State {
    packages: BTreeMap<String, Published>,
    files: HashMap<String, Vec<u8>>,
    log: Vec<(String, HashMap<String, String>)>,
    fail_once: HashSet<String>,
    /// A path prefix answered 401 without this bearer token.
    guarded: Option<(String, String)>,
}

struct Registry {
    base: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
}

impl Registry {
    fn start() -> Registry {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_state, thread_stop) = (Arc::clone(&state), Arc::clone(&stop));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let state = Arc::clone(&thread_state);
                std::thread::spawn(move || serve(stream, &state));
            }
        });
        Registry {
            base: format!("http://{addr}/"),
            state,
            stop,
            addr,
        }
    }

    fn url(&self) -> Url {
        Url::parse(&self.base).unwrap()
    }

    /// Publishes `name@version`: `manifest` is merged into its package.json, and `files` are
    /// its other files (an `index.js` naming it when none are given).
    fn publish_files(
        &self,
        name: &str,
        version: &str,
        manifest: Value,
        files: Vec<(String, Vec<u8>)>,
    ) {
        let mut package = json!({ "name": name, "version": version });
        for (key, value) in manifest.as_object().cloned().unwrap_or_default() {
            package[key] = value;
        }
        let mut all = vec![(
            "package.json".to_owned(),
            serde_json::to_vec_pretty(&package).unwrap(),
        )];
        if files.is_empty() {
            all.push((
                "index.js".to_owned(),
                format!("module.exports = {:?};", format!("{name}@{version}")).into_bytes(),
            ));
        }
        all.extend(files);
        let bytes = tarball(&all);
        let integrity = sri(&bytes);
        self.publish_tarball(name, version, package, bytes, Some(integrity));
    }

    fn publish(&self, name: &str, version: &str, manifest: Value) {
        self.publish_files(name, version, manifest, Vec::new());
    }

    /// Publishes a tarball as it is, with the integrity the packument claims for it.
    fn publish_tarball(
        &self,
        name: &str,
        version: &str,
        package: Value,
        bytes: Vec<u8>,
        integrity: Option<String>,
    ) {
        let base = name.rsplit('/').next().unwrap();
        let path = format!("/{name}/-/{base}-{version}.tgz");
        let mut doc = package;
        doc["name"] = json!(name);
        doc["version"] = json!(version);
        doc["dist"] =
            json!({ "tarball": format!("http://{}{path}", self.addr), "shasum": sha1_hex(&bytes) });
        if let Some(integrity) = integrity {
            doc["dist"]["integrity"] = json!(integrity);
        }
        let mut state = self.state.lock().unwrap();
        state.files.insert(path, bytes);
        let (tags, versions) = state.packages.entry(name.to_owned()).or_default();
        versions.insert(version.to_owned(), doc);
        if !version.contains('-') {
            tags.insert("latest".to_owned(), version.to_owned());
        }
    }

    fn tag(&self, name: &str, tag: &str, version: &str) {
        let mut state = self.state.lock().unwrap();
        state
            .packages
            .get_mut(name)
            .unwrap()
            .0
            .insert(tag.to_owned(), version.to_owned());
    }

    fn requests(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .log
            .iter()
            .map(|(path, _)| path.clone())
            .collect()
    }

    fn clear_log(&self) {
        self.state.lock().unwrap().log.clear();
    }

    fn header_of(&self, path: &str, header: &str) -> Option<String> {
        let state = self.state.lock().unwrap();
        state
            .log
            .iter()
            .find(|(p, _)| p == path)
            .and_then(|(_, h)| h.get(header).cloned())
    }

    fn options(&self) -> InstallOptions {
        InstallOptions {
            registry: self.url(),
            retries: 1,
            concurrency: 4,
            platform: linux_x64(),
            ..InstallOptions::default()
        }
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }
}

fn linux_x64() -> Platform {
    Platform {
        os: "linux".into(),
        cpu: "x64".into(),
        libc: Some("glibc".into()),
    }
}

fn serve(mut stream: TcpStream, state: &Mutex<State>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.lines();
    let path = lines
        .next()
        .unwrap_or("")
        .split(' ')
        .nth(1)
        .unwrap_or("/")
        .to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let (status, body) = {
        let mut state = state.lock().unwrap();
        state.log.push((path.clone(), headers.clone()));
        let denied = state.guarded.as_ref().is_some_and(|(prefix, token)| {
            path.starts_with(prefix.as_str())
                && headers.get("authorization") != Some(&format!("Bearer {token}"))
        });
        if state.fail_once.remove(&path) {
            (503, b"busy".to_vec())
        } else if denied {
            (401, b"unauthorized".to_vec())
        } else if let Some(bytes) = state.files.get(&path) {
            (200, bytes.clone())
        } else {
            let name = path
                .trim_start_matches('/')
                .replace("%2f", "/")
                .replace("%2F", "/");
            match state.packages.get(&name) {
                Some((tags, versions)) if !path.contains("/-/") => (
                    200,
                    serde_json::to_vec(
                        &json!({ "name": name, "dist-tags": tags, "versions": versions }),
                    )
                    .unwrap(),
                ),
                _ => (404, b"{\"error\":\"not found\"}".to_vec()),
            }
        }
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
}

// ---------------------------------------------------------------------------------------------
// Projects.

fn project(manifest: Value) -> TempDir {
    let dir = TempDir::new("project");
    write_manifest(dir.path(), &manifest);
    dir
}

fn write_manifest(dir: &Path, manifest: &Value) {
    let mut manifest = manifest.clone();
    if manifest.get("name").is_none() {
        manifest["name"] = json!("project");
        manifest["version"] = json!("1.0.0");
    }
    std::fs::write(
        dir.join("package.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

/// The version of the package installed at `location`, if one is.
fn version_at(dir: &Path, location: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(location).join("package.json")).ok()?;
    let doc: Value = serde_json::from_str(&text).ok()?;
    doc["version"].as_str().map(str::to_owned)
}

fn lockfile(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("package-lock.json")).unwrap()).unwrap()
}

/// Every package folder under `node_modules`, at any depth, as locations.
fn layout(dir: &Path) -> Vec<String> {
    fn walk(at: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(at.join("node_modules")) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let mut found = Vec::new();
            if name.starts_with('@') {
                for inner in std::fs::read_dir(entry.path()).unwrap().flatten() {
                    found.push((
                        format!("{name}/{}", inner.file_name().to_string_lossy()),
                        inner.path(),
                    ));
                }
            } else {
                found.push((name, entry.path()));
            }
            for (name, path) in found {
                let location = format!("{prefix}node_modules/{name}");
                out.push(location.clone());
                walk(&path, &format!("{location}/"), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------------
// Resolution.

#[test]
fn ranges_tags_prereleases_aliases_and_scopes_resolve_as_npm_does() {
    let registry = Registry::start();
    for v in ["1.0.0", "1.5.0", "2.0.0"] {
        registry.publish("tagged", v, json!({}));
    }
    registry.tag("tagged", "latest", "1.0.0");
    registry.publish("pre", "1.0.0", json!({}));
    registry.publish("pre", "1.1.0-beta.1", json!({}));
    registry.publish("pre", "1.1.0-beta.2", json!({}));
    registry.publish("nexty", "1.0.0", json!({}));
    registry.publish("nexty", "3.0.0-rc.1", json!({}));
    registry.tag("nexty", "next", "3.0.0-rc.1");
    for v in ["1.0.0", "1.0.5", "1.9.0", "2.0.0"] {
        registry.publish("ranged", v, json!({}));
    }
    registry.publish("exact", "1.0.0", json!({}));
    registry.publish("exact", "1.1.0", json!({}));
    registry.publish("@scope/real", "2.1.0", json!({}));
    registry.publish("@scope/real", "3.0.0", json!({}));
    registry.publish("noprerelease", "1.0.0", json!({}));
    registry.publish("noprerelease", "1.1.0-alpha", json!({}));

    let dir = project(json!({ "dependencies": {
        "tagged": "^1.0.0",
        "pre": "^1.1.0-beta.0",
        "nexty": "next",
        "ranged": "~1.0.0 || >=1.5 <2",
        "exact": "1.0.0",
        "aliased": "npm:@scope/real@^2",
        "@scope/real": "*",
        "noprerelease": "*",
    }}));
    let report = install(dir.path(), &registry.options()).unwrap();
    let at = |location: &str| version_at(dir.path(), location);
    assert_eq!(
        at("node_modules/tagged").as_deref(),
        Some("1.0.0"),
        "the latest tag wins when it satisfies"
    );
    assert_eq!(at("node_modules/pre").as_deref(), Some("1.1.0-beta.2"));
    assert_eq!(at("node_modules/nexty").as_deref(), Some("3.0.0-rc.1"));
    assert_eq!(at("node_modules/ranged").as_deref(), Some("1.9.0"));
    assert_eq!(at("node_modules/exact").as_deref(), Some("1.0.0"));
    assert_eq!(at("node_modules/aliased").as_deref(), Some("2.1.0"));
    assert_eq!(at("node_modules/@scope/real").as_deref(), Some("3.0.0"));
    assert_eq!(at("node_modules/noprerelease").as_deref(), Some("1.0.0"));
    assert_eq!(report.installed.len(), 8);
    assert!(report.downloaded_bytes > 0);
    assert!(report.tarball_bytes > 0 && report.unpacked_bytes > 0);

    // A scope's `/` is escaped in the request, and each packument is asked for once.
    let requests = registry.requests();
    assert!(
        requests.contains(&"/@scope%2freal".to_owned()),
        "{requests:?}"
    );
    assert_eq!(
        requests.iter().filter(|p| *p == "/@scope%2freal").count(),
        1
    );
    assert_eq!(
        registry.header_of("/tagged", "accept").as_deref(),
        Some("application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8, */*")
    );

    let lock = lockfile(dir.path());
    assert_eq!(lock["lockfileVersion"], 3);
    assert_eq!(
        lock["packages"]["node_modules/aliased"]["name"],
        "@scope/real"
    );
    assert_eq!(
        lock["packages"][""]["dependencies"]["aliased"],
        "npm:@scope/real@^2"
    );
    let tagged = &lock["packages"]["node_modules/tagged"];
    assert_eq!(tagged["version"], "1.0.0");
    assert!(
        tagged["resolved"]
            .as_str()
            .unwrap()
            .ends_with("/tagged/-/tagged-1.0.0.tgz")
    );
    assert!(tagged["integrity"].as_str().unwrap().starts_with("sha512-"));
}

#[test]
fn packages_hoist_dedupe_and_nest_only_on_conflict() {
    let registry = Registry::start();
    registry.publish(
        "a",
        "1.0.0",
        json!({ "dependencies": { "shared": "^1.0.0", "conflict": "^1.0.0", "deep": "^1" } }),
    );
    registry.publish(
        "b",
        "1.0.0",
        json!({ "dependencies": { "shared": "^1.1.0", "conflict": "^2.0.0" } }),
    );
    registry.publish(
        "deep",
        "1.0.0",
        json!({ "dependencies": { "shared": "^1.0.0" } }),
    );
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        registry.publish("shared", v, json!({}));
    }
    registry.publish("conflict", "1.0.0", json!({}));
    registry.publish("conflict", "2.0.0", json!({}));
    // p's q@1 nests under p; q@1's z@2 cannot go up to p, where it would hide the root z@1 that
    // p itself needs, so it nests under q.
    registry.publish(
        "p",
        "1.0.0",
        json!({ "dependencies": { "q": "^1", "z": "^1" } }),
    );
    registry.publish("q", "1.0.0", json!({ "dependencies": { "z": "^2" } }));
    registry.publish("q", "2.0.0", json!({}));
    registry.publish("z", "1.0.0", json!({}));
    registry.publish("z", "2.0.0", json!({}));

    let dir = project(
        json!({ "dependencies": { "a": "^1", "b": "^1", "p": "^1", "q": "^2", "z": "^1" } }),
    );
    install(dir.path(), &registry.options()).unwrap();
    assert_eq!(
        layout(dir.path()),
        [
            "node_modules/a",
            "node_modules/b",
            "node_modules/b/node_modules/conflict",
            "node_modules/conflict",
            "node_modules/deep",
            "node_modules/p",
            "node_modules/p/node_modules/q",
            "node_modules/p/node_modules/q/node_modules/z",
            "node_modules/q",
            "node_modules/shared",
            "node_modules/z",
        ]
    );
    let at = |location: &str| version_at(dir.path(), location).unwrap();
    assert_eq!(
        at("node_modules/shared"),
        "1.2.0",
        "one copy serves every dependent"
    );
    assert_eq!(
        at("node_modules/conflict"),
        "1.0.0",
        "first come is hoisted"
    );
    assert_eq!(at("node_modules/b/node_modules/conflict"), "2.0.0");
    assert_eq!(at("node_modules/p/node_modules/q"), "1.0.0");
    assert_eq!(at("node_modules/p/node_modules/q/node_modules/z"), "2.0.0");
    assert_eq!(at("node_modules/z"), "1.0.0");
    let requests = registry.requests();
    assert_eq!(requests.iter().filter(|p| *p == "/shared").count(), 1);
    // Every lock entry records its dependencies as published.
    let lock = lockfile(dir.path());
    assert_eq!(
        lock["packages"]["node_modules/b"]["dependencies"]["conflict"],
        "^2.0.0"
    );
}

#[test]
fn peers_are_not_installed_with_legacy_peer_deps() {
    let registry = Registry::start();
    registry.publish(
        "plugin",
        "1.0.0",
        json!({
            "dependencies": { "helper": "^1" },
            "peerDependencies": { "host": "^1", "helper": "^1" },
            "peerDependenciesMeta": { "host": { "optional": false } },
            "scripts": { "postinstall": "node -e \"require('fs').writeFileSync('PWNED','')\"" },
            "hasInstallScript": true
        }),
    );
    registry.publish("helper", "1.0.0", json!({}));
    registry.publish("host", "1.0.0", json!({}));
    let dir = project(json!({ "dependencies": { "plugin": "^1" } }));
    install(dir.path(), &registry.options()).unwrap();
    assert_eq!(
        layout(dir.path()),
        ["node_modules/helper", "node_modules/plugin"]
    );
    assert!(
        !registry.requests().contains(&"/host".to_owned()),
        "a peer is not even looked up"
    );
    assert!(
        !dir.path().join("PWNED").exists()
            && !dir.path().join("node_modules/plugin/PWNED").exists()
    );
    let lock = lockfile(dir.path());
    assert_eq!(
        lock["packages"]["node_modules/plugin"]["peerDependencies"]["host"],
        "^1"
    );
    assert_eq!(
        lock["packages"]["node_modules/plugin"]["hasInstallScript"],
        true
    );
}

#[test]
fn optional_and_foreign_platform_packages_are_skipped_with_reasons() {
    let registry = Registry::start();
    registry.publish(
        "winonly",
        "1.0.0",
        json!({ "os": ["win32"], "dependencies": { "winhelper": "^1" } }),
    );
    registry.publish("winhelper", "1.0.0", json!({}));
    registry.publish("notx64", "1.0.0", json!({ "cpu": ["!x64"] }));
    registry.publish("musl", "1.0.0", json!({ "libc": ["musl"] }));
    registry.publish(
        "anyos",
        "1.0.0",
        json!({ "os": ["linux", "darwin", "win32"], "cpu": ["x64", "arm64"] }),
    );
    registry.publish(
        "needsopt",
        "1.0.0",
        json!({ "optionalDependencies": { "notx64": "^1", "gone": "^1" } }),
    );
    registry.publish(
        "brokenopt",
        "1.0.0",
        json!({ "dependencies": { "absent": "^1" } }),
    );
    let dir = project(json!({
        "dependencies": { "needsopt": "^1", "anyos": "^1" },
        "optionalDependencies": { "missing": "^1", "winonly": "^1", "musl": "^1", "brokenopt": "^1" }
    }));
    let report = install(dir.path(), &registry.options()).unwrap();
    assert_eq!(
        layout(dir.path()),
        ["node_modules/anyos", "node_modules/needsopt"]
    );
    let reason_for = |name: &str| {
        report
            .skipped
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} is not reported skipped: {:?}", report.skipped))
            .reason
            .clone()
    };
    assert!(
        reason_for("missing").contains("not in the registry"),
        "{}",
        reason_for("missing")
    );
    assert!(reason_for("gone").contains("not in the registry"));
    assert!(
        reason_for("winonly").contains("os win32"),
        "{}",
        reason_for("winonly")
    );
    assert!(reason_for("notx64").contains("cpu !x64"));
    assert!(reason_for("musl").contains("libc musl"));
    assert!(
        reason_for("brokenopt").contains("absent"),
        "{}",
        reason_for("brokenopt")
    );

    // The platform-specific packages stay in the lockfile, as npm keeps them for other machines.
    let lock = lockfile(dir.path());
    let winonly = &lock["packages"]["node_modules/winonly"];
    assert_eq!(winonly["optional"], true);
    assert_eq!(winonly["os"], json!(["win32"]));
    assert_eq!(lock["packages"]["node_modules/winhelper"]["optional"], true);
    assert!(lock["packages"].get("node_modules/brokenopt").is_none());
    assert!(
        lock["packages"]["node_modules/anyos"]
            .get("optional")
            .is_none()
    );

    // On the platform they are for, the same lockfile installs them.
    let mut windows = registry.options();
    windows.platform = Platform {
        os: "win32".into(),
        cpu: "arm64".into(),
        libc: None,
    };
    std::fs::remove_dir_all(dir.path().join("node_modules")).unwrap();
    let report = install(dir.path(), &windows).unwrap();
    assert!(report.from_lockfile);
    assert!(layout(dir.path()).contains(&"node_modules/winonly".to_owned()));
    assert!(layout(dir.path()).contains(&"node_modules/notx64".to_owned()));
}

#[test]
fn a_required_package_for_another_platform_fails_the_install() {
    let registry = Registry::start();
    registry.publish("macos", "1.0.0", json!({ "os": ["darwin"] }));
    let dir = project(json!({ "dependencies": { "macos": "^1" } }));
    let error = install(dir.path(), &registry.options()).unwrap_err();
    assert!(
        matches!(error, InstallError::BadPlatform { .. }),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "macos@1.0.0 is not for this platform: it is for os darwin and this is linux"
    );
    assert!(!dir.path().join("node_modules/macos").exists());
}

// ---------------------------------------------------------------------------------------------
// Tarballs.

#[test]
fn a_tarball_failing_its_integrity_check_writes_nothing() {
    let registry = Registry::start();
    registry.publish("good", "1.0.0", json!({}));
    let bytes = tarball(&[(
        "package.json".into(),
        br#"{"name":"bad","version":"1.0.0"}"#.to_vec(),
    )]);
    let wrong = sri(b"something else");
    registry.publish_tarball(
        "bad",
        "1.0.0",
        json!({}),
        bytes.clone(),
        Some(wrong.clone()),
    );
    let dir = project(json!({ "dependencies": { "bad": "1.0.0", "good": "1.0.0" } }));
    let error = install(dir.path(), &registry.options()).unwrap_err();
    match &error {
        InstallError::Integrity {
            package,
            version,
            expected,
            actual,
        } => {
            assert_eq!((package.as_str(), version.as_str()), ("bad", "1.0.0"));
            assert_eq!(expected, &wrong);
            assert_eq!(actual, &sri(&bytes));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        error
            .to_string()
            .starts_with("bad@1.0.0: the tarball's integrity check failed")
    );
    assert_eq!(
        layout(dir.path()),
        Vec::<String>::new(),
        "nothing is moved into place"
    );
    assert!(!dir.path().join("node_modules/.verve-staging").exists());
    assert!(!dir.path().join("package-lock.json").exists());

    // With only the old SHA-1 shasum, that is checked instead.
    let registry = Registry::start();
    let bytes = tarball(&[(
        "package.json".into(),
        br#"{"name":"old","version":"1.0.0"}"#.to_vec(),
    )]);
    registry.publish_tarball("old", "1.0.0", json!({}), bytes.clone(), None);
    let dir = project(json!({ "dependencies": { "old": "1.0.0" } }));
    install(dir.path(), &registry.options()).unwrap();
    assert_eq!(
        lockfile(dir.path())["packages"]["node_modules/old"]["integrity"],
        sri(&bytes)
    );
}

#[test]
fn a_tarball_escaping_its_folder_is_refused() {
    let registry = Registry::start();
    let dir = project(json!({ "dependencies": { "evil": "1.0.0" } }));
    for (label, entry) in [
        ("dotdot", "package/../../escaped.txt"),
        ("absolute", "/escaped.txt"),
    ] {
        let bytes = raw_tarball(&[
            (
                "package/package.json",
                br#"{"name":"evil","version":"1.0.0"}"#,
            ),
            (entry, b"gotcha"),
        ]);
        let integrity = sri(&bytes);
        registry.publish_tarball("evil", "1.0.0", json!({}), bytes, Some(integrity));
        let error = install(dir.path(), &registry.options()).unwrap_err();
        match &error {
            InstallError::UnsafeTarball {
                package,
                entry: named,
                ..
            } => {
                assert_eq!(package, "evil", "{label}");
                assert_eq!(named, entry);
            }
            other => panic!("{label}: {other:?}"),
        }
        assert!(
            error.to_string().contains("refusing the package"),
            "{error}"
        );
        assert!(!dir.path().join("escaped.txt").exists());
        assert!(!dir.path().parent().unwrap().join("escaped.txt").exists());
        assert!(!dir.path().join("node_modules/evil").exists());
    }
}

#[test]
fn bundled_dependencies_come_from_the_bundler_tarball() {
    let registry = Registry::start();
    let inner_package =
        br#"{"name":"inner","version":"4.5.6","dependencies":{"innerdep":"^1"}}"#.to_vec();
    registry.publish_files(
        "bundler",
        "1.0.0",
        json!({ "dependencies": { "inner": "^4", "plain": "^1" }, "bundleDependencies": ["inner"] }),
        vec![
            ("index.js".into(), b"module.exports = require('inner');".to_vec()),
            ("node_modules/inner/package.json".into(), inner_package),
            ("node_modules/inner/index.js".into(), b"module.exports = 'inner';".to_vec()),
            ("node_modules/innerdep/package.json".into(), br#"{"name":"innerdep","version":"1.0.0"}"#.to_vec()),
        ],
    );
    registry.publish("plain", "1.0.0", json!({}));
    let dir = project(json!({ "dependencies": { "bundler": "^1" } }));
    install(dir.path(), &registry.options()).unwrap();
    assert_eq!(
        layout(dir.path()),
        [
            "node_modules/bundler",
            "node_modules/bundler/node_modules/inner",
            "node_modules/bundler/node_modules/innerdep",
            "node_modules/plain",
        ]
    );
    let requests = registry.requests();
    assert!(
        !requests.iter().any(|p| p.contains("inner")),
        "bundled packages are never fetched: {requests:?}"
    );
    let lock = lockfile(dir.path());
    let inner = &lock["packages"]["node_modules/bundler/node_modules/inner"];
    assert_eq!(inner["inBundle"], true);
    assert_eq!(inner["version"], "4.5.6");
    assert!(inner.get("resolved").is_none());
    assert_eq!(
        lock["packages"]["node_modules/bundler"]["bundleDependencies"],
        json!(["inner"])
    );

    // Installed again from that lockfile, the bundle still comes with its bundler.
    std::fs::remove_dir_all(dir.path().join("node_modules")).unwrap();
    registry.clear_log();
    let report = install(dir.path(), &registry.options()).unwrap();
    assert!(report.from_lockfile);
    assert!(
        dir.path()
            .join("node_modules/bundler/node_modules/inner/index.js")
            .is_file()
    );
    assert!(!registry.requests().iter().any(|p| p.contains("inner")));
}

#[test]
#[cfg(unix)]
fn executable_bits_survive_unpacking() {
    use std::os::unix::fs::PermissionsExt;
    let registry = Registry::start();
    registry.publish_files(
        "tool",
        "1.0.0",
        json!({ "bin": { "tool": "bin/tool" } }),
        vec![
            ("bin/tool".into(), b"#!/bin/sh\necho hi\n".to_vec()),
            ("lib.js".into(), b"".to_vec()),
        ],
    );
    let dir = project(json!({ "dependencies": { "tool": "1.0.0" } }));
    install(dir.path(), &registry.options()).unwrap();
    let mode = |p: &str| {
        std::fs::metadata(dir.path().join(p))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("node_modules/tool/bin/tool"), 0o755);
    assert_eq!(mode("node_modules/tool/lib.js"), 0o644);
}

// ---------------------------------------------------------------------------------------------
// Lockfile, cache and re-runs.

fn three_package_registry() -> Registry {
    let registry = Registry::start();
    registry.publish("top", "1.0.0", json!({ "dependencies": { "mid": "^1" } }));
    registry.publish(
        "mid",
        "1.0.0",
        json!({ "dependencies": { "@s/leaf": "^1" } }),
    );
    registry.publish("@s/leaf", "1.0.0", json!({}));
    registry
}

#[test]
fn a_lockfile_installs_exactly_without_metadata_and_offline_from_the_cache() {
    let cache = TempDir::new("cache");
    let registry = three_package_registry();
    let dir = project(json!({ "dependencies": { "top": "^1" } }));
    let mut options = registry.options();
    options.cache_dir = Some(cache.path().to_owned());
    let first = install(dir.path(), &options).unwrap();
    assert!(!first.from_lockfile);
    assert_eq!(
        first.lockfile.as_deref(),
        Some(dir.path().join("package-lock.json").as_path())
    );

    // A newer version published since does not move a locked install.
    registry.publish(
        "mid",
        "1.5.0",
        json!({ "dependencies": { "@s/leaf": "^1" } }),
    );
    std::fs::remove_dir_all(dir.path().join("node_modules")).unwrap();
    registry.clear_log();
    let mut no_cache = registry.options();
    no_cache.cache_dir = None;
    let report = install(dir.path(), &no_cache).unwrap();
    assert!(report.from_lockfile);
    assert_eq!(
        report.lockfile, None,
        "an unchanged lockfile is not rewritten"
    );
    assert_eq!(
        version_at(dir.path(), "node_modules/mid").as_deref(),
        Some("1.0.0")
    );
    let requests = registry.requests();
    assert!(
        requests.iter().all(|p| p.ends_with(".tgz")),
        "only tarballs: {requests:?}"
    );

    // With the registry gone, offline, everything comes from the lockfile and the cache.
    drop(registry);
    std::fs::remove_dir_all(dir.path().join("node_modules")).unwrap();
    let mut offline = options.clone();
    offline.offline = true;
    let report = install(dir.path(), &offline).unwrap();
    assert!(report.from_lockfile);
    assert_eq!(report.installed.len(), 3);
    assert_eq!(report.downloaded_bytes, 0);
    assert_eq!(
        layout(dir.path()),
        [
            "node_modules/@s/leaf",
            "node_modules/mid",
            "node_modules/top"
        ]
    );

    // Offline with an empty cache says what is missing.
    std::fs::remove_dir_all(dir.path().join("node_modules")).unwrap();
    let empty = TempDir::new("empty-cache");
    offline.cache_dir = Some(empty.path().to_owned());
    let error = install(dir.path(), &offline).unwrap_err();
    assert!(matches!(error, InstallError::Offline { .. }), "{error:?}");
    assert!(error.to_string().contains("not in the cache"), "{error}");
}

#[test]
fn a_lockfile_that_no_longer_matches_is_resolved_again_keeping_its_versions() {
    let registry = three_package_registry();
    registry.publish("extra", "1.0.0", json!({}));
    let dir = project(json!({ "dependencies": { "top": "^1" } }));
    install(dir.path(), &registry.options()).unwrap();
    registry.publish(
        "mid",
        "1.5.0",
        json!({ "dependencies": { "@s/leaf": "^1" } }),
    );
    write_manifest(
        dir.path(),
        &json!({ "dependencies": { "top": "^1", "extra": "^1" } }),
    );
    let report = install(dir.path(), &registry.options()).unwrap();
    assert!(!report.from_lockfile);
    assert_eq!(
        version_at(dir.path(), "node_modules/mid").as_deref(),
        Some("1.0.0"),
        "the locked version is kept"
    );
    assert_eq!(
        version_at(dir.path(), "node_modules/extra").as_deref(),
        Some("1.0.0")
    );
    let installed: Vec<&str> = report
        .installed
        .iter()
        .map(|p| p.location.as_str())
        .collect();
    assert_eq!(installed, ["node_modules/extra"], "the rest stays in place");
    assert_eq!(
        lockfile(dir.path())["packages"][""]["dependencies"]["extra"],
        "^1"
    );
}

#[test]
fn a_second_run_changes_nothing_and_a_removed_dependency_is_pruned() {
    let registry = three_package_registry();
    registry.publish("other", "1.0.0", json!({}));
    let dir = project(json!({ "dependencies": { "top": "^1", "other": "^1" } }));
    let first = install(dir.path(), &registry.options()).unwrap();
    assert_eq!(first.installed.len(), 4);
    let lock_before = std::fs::read_to_string(dir.path().join("package-lock.json")).unwrap();

    registry.clear_log();
    let second = install(dir.path(), &registry.options()).unwrap();
    assert!(second.installed.is_empty(), "{:?}", second.installed);
    assert_eq!(second.unchanged.len(), 4);
    assert!(registry.requests().is_empty(), "{:?}", registry.requests());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("package-lock.json")).unwrap(),
        lock_before
    );

    // A package deleted by hand is put back; the others are left alone.
    std::fs::remove_dir_all(dir.path().join("node_modules/mid")).unwrap();
    let third = install(dir.path(), &registry.options()).unwrap();
    let put_back: Vec<&str> = third
        .installed
        .iter()
        .map(|p| p.location.as_str())
        .collect();
    assert_eq!(put_back, ["node_modules/mid"]);

    write_manifest(dir.path(), &json!({ "dependencies": { "top": "^1" } }));
    let fourth = install(dir.path(), &registry.options()).unwrap();
    assert_eq!(fourth.removed, ["node_modules/other"]);
    assert!(!dir.path().join("node_modules/other").exists());
    assert!(
        lockfile(dir.path())["packages"]
            .get("node_modules/other")
            .is_none()
    );
}

// ---------------------------------------------------------------------------------------------
// Errors, retries, tokens.

#[test]
fn errors_name_the_package_and_the_reason() {
    let registry = Registry::start();
    registry.publish("dep", "1.0.0", json!({ "dependencies": { "ghost": "^1" } }));
    registry.publish("old", "1.0.0", json!({}));
    let options = registry.options();

    let dir = project(json!({ "dependencies": { "dep": "^1" } }));
    let error = install(dir.path(), &options).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "ghost is not in the registry (404 from {}ghost) (required by node_modules/dep)",
            registry.base
        )
    );

    let dir = project(json!({ "dependencies": { "old": "^2" } }));
    let error = install(dir.path(), &options).unwrap_err();
    assert_eq!(
        error.to_string(),
        "old@^2: no version matches (published: 1.0.0) (required by the project)"
    );

    let dir = project(json!({ "dependencies": { "fromgit": "github:someone/repo" } }));
    let error = install(dir.path(), &options).unwrap_err();
    let text = error.to_string();
    assert!(
        text.starts_with("fromgit@github:someone/repo cannot be installed:"),
        "{text}"
    );
    assert!(text.contains("git dependency"), "{text}");

    let dir = project(json!({ "dependencies": { "local": "file:../local" } }));
    let error = install(dir.path(), &options).unwrap_err();
    assert!(error.to_string().contains("local path"), "{error}");

    let dir = TempDir::new("no-manifest");
    let error = install(dir.path(), &options).unwrap_err();
    assert!(matches!(error, InstallError::Project { .. }), "{error:?}");
}

#[test]
fn server_errors_are_retried_and_tokens_go_to_their_registry_only() {
    let registry = Registry::start();
    registry.publish("flaky", "1.0.0", json!({}));
    registry.publish("@private/pkg", "1.0.0", json!({}));
    {
        let mut state = registry.state.lock().unwrap();
        state.fail_once.insert("/flaky".into());
        state.fail_once.insert("/flaky/-/flaky-1.0.0.tgz".into());
        state.guarded = Some(("/@private".into(), "s3cret".into()));
    }
    let private = registry.url();
    let mut options = registry.options();
    // The default registry is elsewhere; only the scope's registry gets the token.
    options.registry = Url::parse("http://127.0.0.1:9/").unwrap();
    options
        .scoped_registries
        .insert("@private".into(), private.clone());
    options.tokens.insert(private, "s3cret".into());
    let dir = project(json!({ "dependencies": { "@private/pkg": "^1" } }));
    install(dir.path(), &options).unwrap();
    assert_eq!(
        registry
            .header_of("/@private%2fpkg", "authorization")
            .as_deref(),
        Some("Bearer s3cret")
    );

    let dir = project(json!({ "dependencies": { "flaky": "^1" } }));
    let report = install(dir.path(), &registry.options()).unwrap();
    assert_eq!(report.installed.len(), 1);
    let requests = registry.requests();
    assert_eq!(
        requests.iter().filter(|p| *p == "/flaky").count(),
        2,
        "{requests:?}"
    );
    assert!(
        registry.header_of("/flaky", "authorization").is_none(),
        "no token outside its registry"
    );

    // Without retries, the 503 is the error.
    registry
        .state
        .lock()
        .unwrap()
        .fail_once
        .insert("/flaky".into());
    let mut once = registry.options();
    once.retries = 0;
    once.use_lockfile = false;
    let dir = project(json!({ "dependencies": { "flaky": "^1" } }));
    let error = install(dir.path(), &once).unwrap_err();
    assert!(error.to_string().contains("HTTP 503"), "{error}");
}

#[test]
fn a_tarball_url_dependency_is_fetched_and_locked() {
    let registry = Registry::start();
    registry.publish(
        "viaurl",
        "3.2.1",
        json!({ "dependencies": { "dep": "^1" } }),
    );
    registry.publish("dep", "1.0.0", json!({}));
    let url = format!("{}viaurl/-/viaurl-3.2.1.tgz", registry.base);
    let dir = project(json!({ "dependencies": { "renamed": url } }));
    install(dir.path(), &registry.options()).unwrap();
    assert_eq!(
        version_at(dir.path(), "node_modules/renamed").as_deref(),
        Some("3.2.1")
    );
    assert_eq!(
        version_at(dir.path(), "node_modules/dep").as_deref(),
        Some("1.0.0")
    );
    let lock = lockfile(dir.path());
    assert_eq!(lock["packages"]["node_modules/renamed"]["resolved"], url);
    assert_eq!(lock["packages"]["node_modules/renamed"]["name"], "viaurl");
    let tarball_requests = registry
        .requests()
        .iter()
        .filter(|p| p.ends_with("viaurl-3.2.1.tgz"))
        .count();
    assert_eq!(
        tarball_requests, 1,
        "fetched once, for resolving and unpacking"
    );
}

#[test]
fn dev_dependencies_are_left_out_unless_asked_for() {
    let registry = Registry::start();
    registry.publish("prod", "1.0.0", json!({}));
    registry.publish("devonly", "1.0.0", json!({}));
    let dir = project(
        json!({ "dependencies": { "prod": "^1" }, "devDependencies": { "devonly": "^1" } }),
    );
    install(dir.path(), &registry.options()).unwrap();
    assert_eq!(layout(dir.path()), ["node_modules/prod"]);
    let mut with_dev = registry.options();
    with_dev.include_dev = true;
    install(dir.path(), &with_dev).unwrap();
    assert_eq!(
        layout(dir.path()),
        ["node_modules/devonly", "node_modules/prod"]
    );
    assert_eq!(
        lockfile(dir.path())["packages"]["node_modules/devonly"]["dev"],
        true
    );
}

// ---------------------------------------------------------------------------------------------
// Verve's own resolver finds what was installed.

#[test]
fn verve_loads_the_installed_graph_through_esm_and_require() {
    let registry = Registry::start();
    registry.publish_files(
        "esm-only",
        "1.0.0",
        json!({ "type": "module", "exports": { ".": { "import": "./lib/index.js" } } }),
        vec![
            (
                "lib/index.js".into(),
                b"import { x } from './util.js';\nexport default 'esm:' + x;\n".to_vec(),
            ),
            ("lib/util.js".into(), b"export const x = 1;\n".to_vec()),
        ],
    );
    registry.publish_files(
        "dual",
        "2.0.0",
        json!({ "exports": { ".": { "import": "./index.mjs", "require": "./index.cjs" }, "./package.json": "./package.json" } }),
        vec![
            ("index.mjs".into(), b"export default 'dual-import';\n".to_vec()),
            ("index.cjs".into(), b"module.exports = 'dual-require';\n".to_vec()),
        ],
    );
    registry.publish_files(
        "dual",
        "1.0.0",
        json!({ "main": "main.js" }),
        vec![("main.js".into(), b"module.exports = 'dual-v1';\n".to_vec())],
    );
    registry.tag("dual", "latest", "2.0.0");
    registry.publish_files(
        "cjs-pkg",
        "1.0.0",
        json!({ "main": "lib/main.js", "dependencies": { "cjs-helper": "^1", "dual": "^1" } }),
        vec![(
            "lib/main.js".into(),
            b"const helper = require('cjs-helper');\nexports.kind = 'cjs+' + helper();\nexports.named = 'named';\nexports.dual = require('dual');\n".to_vec(),
        )],
    );
    registry.publish_files(
        "cjs-helper",
        "1.0.0",
        json!({}),
        vec![(
            "index.js".into(),
            b"module.exports = () => 'helper';\n".to_vec(),
        )],
    );
    let dir =
        project(json!({ "dependencies": { "esm-only": "^1", "dual": "^2", "cjs-pkg": "^1" } }));
    install(dir.path(), &registry.options()).unwrap();
    assert!(
        dir.path()
            .join("node_modules/cjs-pkg/node_modules/dual")
            .is_dir()
    );

    let mut runtime = verve::Runtime::new();
    let main = dir.path().join("main.mjs");
    let source = "import esm from 'esm-only';\nimport dual from 'dual';\nimport cjs, { named } from 'cjs-pkg';\n\
                  globalThis.result = { esm, dual, kind: cjs.kind, named, nested: cjs.dual };\n";
    std::fs::write(&main, source).unwrap();
    let promise = runtime.load_esm(&main, source).unwrap();
    assert!(
        matches!(promise.state(), PromiseState::Fulfilled(_)),
        "{:?}",
        promise.state()
    );
    let result = verve::print(&runtime.eval("JSON.stringify(globalThis.result)").unwrap());
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        result,
        json!({ "esm": "esm:1", "dual": "dual-import", "kind": "cjs+helper", "named": "named", "nested": "dual-v1" })
    );

    let main_cjs = dir.path().join("main.cjs");
    std::fs::write(
        &main_cjs,
        "globalThis.required = require('dual') + '|' + require('cjs-pkg').dual;\n",
    )
    .unwrap();
    runtime.require(main_cjs.to_str().unwrap()).unwrap();
    let required = verve::print(&runtime.eval("globalThis.required").unwrap());
    assert_eq!(required, "dual-require|dual-v1");
}

// ---------------------------------------------------------------------------------------------
// The real registry, by hand: `cargo test --test npm -- --ignored --nocapture`.

#[test]
#[ignore = "reaches registry.npmjs.org"]
fn installs_ms_from_the_real_registry() {
    let cache = TempDir::new("real-cache");
    let dir = project(json!({ "dependencies": { "ms": "^2.1.0" } }));
    let options = InstallOptions {
        cache_dir: Some(cache.path().to_owned()),
        ..InstallOptions::default()
    };
    let started = std::time::Instant::now();
    let report = install(dir.path(), &options).unwrap();
    eprintln!("ms: {report:#?} in {:?}", started.elapsed());
    assert!(
        version_at(dir.path(), "node_modules/ms")
            .unwrap()
            .starts_with("2.1.")
    );
    let mut runtime = verve::Runtime::new();
    let main = dir.path().join("main.cjs");
    std::fs::write(&main, "globalThis.days = require('ms')('2 days');\n").unwrap();
    runtime.require(main.to_str().unwrap()).unwrap();
    assert_eq!(
        verve::print(&runtime.eval("globalThis.days").unwrap()),
        "172800000"
    );
}

/// The bare specifiers Rebon's composition provides itself (`provided-modules.json`).
const REBON_PROVIDED: &[&str] = &[
    "cordis",
    "cosmokit",
    "schemastery",
    "@deepseek-ai/cordis",
    "@deepseek-ai/cosmokit",
    "@deepseek-ai/schemastery",
    "@deepseek-ai/cordis-plugin-timer",
    "@deepseek-ai/cordis-plugin-logger-console",
    "@deepseek-ai/dsh-llm-deepseek",
    "@deepseek-ai/dsh-timeout",
    "@deepseek-ai/dsh-llm",
    "@deepseek-ai/dsh-credentials",
    "@deepseek-ai/dsh-settings",
    "@deepseek-ai/dsh-launch-environment",
    "@deepseek-ai/dsh-anonymous-user-id",
    "eventsource-parser",
    "@deepseek-ai/dsh-tools",
    "@deepseek-ai/dsh-tool-todo",
    "zod",
    "@deepseek-ai/dsh-web",
    "@deepseek-ai/dsh-web-search-exa",
    "@deepseek-ai/dsh-tool-web",
    "@deepseek-ai/dsh-scope",
    "@deepseek-ai/dsh-session",
    "@deepseek-ai/dsh-agent",
    "@deepseek-ai/dsh-system-prompt",
    "@deepseek-ai/dsh-agent-loop",
    "@deepseek-ai/dsh-environment",
    "@deepseek-ai/dsh-user-id",
    "rebon",
];

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "node_modules" {
                copy_tree(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Rebon's DSH adapter's shape: the package itself, slimmed to its dependencies and the peers
/// Rebon does not provide, installed in its own folder.
fn install_dsh_as_rebon(package: &str) {
    let cache = TempDir::new("dsh-cache");
    let options = InstallOptions {
        cache_dir: Some(cache.path().to_owned()),
        ..InstallOptions::default()
    };
    let fetch = project(json!({ "dependencies": { package: "latest" } }));
    let report = install(fetch.path(), &options).unwrap();
    eprintln!("fetched {package}: {} packages", report.installed.len());

    let plugin = TempDir::new("dsh-plugin");
    copy_tree(
        &fetch.path().join("node_modules").join(package),
        plugin.path(),
    );
    let original: Value =
        serde_json::from_str(&std::fs::read_to_string(plugin.path().join("package.json")).unwrap())
            .unwrap();
    let mut dependencies = original["dependencies"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (name, range) in original["peerDependencies"]
        .as_object()
        .cloned()
        .unwrap_or_default()
    {
        if !REBON_PROVIDED.contains(&name.as_str()) {
            dependencies.insert(name, range);
        }
    }
    let mut slim = serde_json::Map::new();
    for key in [
        "name",
        "version",
        "type",
        "main",
        "exports",
        "module",
        "description",
        "license",
    ] {
        if let Some(value) = original.get(key) {
            slim.insert(key.to_owned(), value.clone());
        }
    }
    slim.insert("dependencies".into(), Value::Object(dependencies.clone()));
    std::fs::write(
        plugin.path().join("package.json"),
        serde_json::to_vec_pretty(&slim).unwrap(),
    )
    .unwrap();
    eprintln!("slim dependencies of {package}: {dependencies:?}");

    let options = InstallOptions {
        write_lockfile: false,
        ..options
    };
    let started = std::time::Instant::now();
    let report = install(plugin.path(), &options).unwrap();
    eprintln!(
        "installed {} packages ({} skipped, {} tarball bytes, {} unpacked) in {:?}",
        report.installed.len(),
        report.skipped.len(),
        report.tarball_bytes,
        report.unpacked_bytes,
        started.elapsed()
    );
    for package in &report.installed {
        eprintln!(
            "  {} {}@{}",
            package.location, package.name, package.version
        );
    }
    for skipped in &report.skipped {
        eprintln!(
            "  skipped {}@{}: {}",
            skipped.name, skipped.wanted, skipped.reason
        );
    }
    for name in dependencies.keys() {
        assert!(
            plugin
                .path()
                .join("node_modules")
                .join(name)
                .join("package.json")
                .is_file(),
            "{name}"
        );
    }
}

#[test]
#[ignore = "reaches registry.npmjs.org"]
fn installs_a_dsh_package_shaped_as_rebon_slims_it() {
    install_dsh_as_rebon("@deepseek-ai/dsh-tool-todo");
}

#[test]
#[ignore = "reaches registry.npmjs.org"]
fn installs_a_dsh_package_with_its_own_dependencies() {
    install_dsh_as_rebon("@deepseek-ai/dsh-web-search-perplexity");
}

#[test]
fn pack_fetches_one_checked_tarball_as_npm_pack_does() {
    let registry = Registry::start();
    registry.publish("plain", "1.0.0", json!({}));
    registry.publish("plain", "1.4.0", json!({}));
    registry.publish("plain", "2.0.0-beta.1", json!({}));
    registry.tag("plain", "next", "2.0.0-beta.1");
    registry.publish("@scope/thing", "3.1.0", json!({}));
    let options = registry.options();
    let dest = TempDir::new("pack");

    // A bare name is its `latest`; a range its highest match; a tag what it points at.
    let latest = pack("plain", &options, dest.path()).unwrap();
    assert_eq!(latest.version, "1.4.0");
    assert!(
        latest.tarball.ends_with("plain-1.4.0.tgz"),
        "{:?}",
        latest.tarball
    );
    assert!(latest.integrity.starts_with("sha512-"));
    assert_eq!(
        pack("plain@~1.0", &options, dest.path()).unwrap().version,
        "1.0.0"
    );
    assert_eq!(
        pack("plain@next", &options, dest.path()).unwrap().version,
        "2.0.0-beta.1"
    );
    // A scope's `@` is part of the name, and npm's file name drops it.
    let scoped = pack("@scope/thing", &options, dest.path()).unwrap();
    assert_eq!(scoped.name, "@scope/thing");
    assert!(scoped.tarball.ends_with("scope-thing-3.1.0.tgz"));

    // The file is the tarball itself, never unpacked.
    let bytes = std::fs::read(&latest.tarball).unwrap();
    assert_eq!(&bytes[..2], &[0x1f, 0x8b]);

    // A tarball URL is fetched as it is, its name read from its own package.json.
    let url = format!("{}plain/-/plain-1.0.0.tgz", registry.base);
    let by_url = pack(&url, &options, dest.path()).unwrap();
    assert_eq!(
        (by_url.name.as_str(), by_url.version.as_str()),
        ("plain", "1.0.0")
    );
}

#[test]
fn pack_refuses_a_tarball_the_registry_did_not_vouch_for() {
    let registry = Registry::start();
    let bytes = tarball(&[(
        "package.json".to_owned(),
        br#"{"name":"forged","version":"1.0.0"}"#.to_vec(),
    )]);
    registry.publish_tarball(
        "forged",
        "1.0.0",
        json!({}),
        bytes,
        Some(sri(b"something else")),
    );
    let dest = TempDir::new("pack-forged");
    let error = pack("forged", &registry.options(), dest.path()).unwrap_err();
    assert!(matches!(error, InstallError::Integrity { .. }), "{error:?}");
    assert!(
        std::fs::read_dir(dest.path()).map_or(true, |mut entries| entries.next().is_none()),
        "nothing is written for a tarball that fails its check"
    );

    let error = pack("ghost", &registry.options(), dest.path()).unwrap_err();
    assert!(matches!(error, InstallError::NotFound { .. }), "{error:?}");
    let error = pack("forged@^9", &registry.options(), dest.path()).unwrap_err();
    assert!(
        matches!(error, InstallError::NoMatchingVersion { .. }),
        "{error:?}"
    );
}
