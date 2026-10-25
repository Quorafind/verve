//! The registry: packuments, tarballs, and the one HTTP client both come through.

use std::collections::{BTreeMap, HashMap};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;

use super::semver::{Range, Version};
use super::spec::Wanted;
use super::{InstallError, InstallOptions, integrity};

/// What the installer needs of one version of a package: from a packument, a lockfile entry
/// or a tarball's own `package.json`.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub dependencies: BTreeMap<String, String>,
    pub optional_dependencies: BTreeMap<String, String>,
    pub peer_dependencies: BTreeMap<String, String>,
    pub peer_dependencies_meta: Option<Value>,
    pub bundled: Vec<String>,
    pub os: Vec<String>,
    pub cpu: Vec<String>,
    pub libc: Vec<String>,
    pub tarball: Option<String>,
    pub integrity: Option<String>,
    pub shasum: Option<String>,
    pub deprecated: bool,
    pub has_install_script: bool,
    pub bin: Option<Value>,
    pub engines: Option<Value>,
    pub license: Option<Value>,
}

fn string_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(one)) => vec![one.clone()],
        _ => Vec::new(),
    }
}

impl Manifest {
    /// Reads a version document: a packument's `versions[v]`, a `package.json`, or a
    /// `package-lock.json` entry (whose `resolved` is the tarball).
    pub fn from_json(doc: &Value) -> Manifest {
        let dependencies = string_map(doc.get("dependencies"));
        let bundled = match doc
            .get("bundleDependencies")
            .or_else(|| doc.get("bundledDependencies"))
        {
            Some(Value::Bool(true)) => dependencies.keys().cloned().collect(),
            other => string_list(other),
        };
        let dist = doc.get("dist");
        let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_owned);
        Manifest {
            name: text(doc.get("name")).unwrap_or_default(),
            version: text(doc.get("version")).unwrap_or_default(),
            optional_dependencies: string_map(doc.get("optionalDependencies")),
            peer_dependencies: string_map(doc.get("peerDependencies")),
            peer_dependencies_meta: doc.get("peerDependenciesMeta").cloned(),
            dependencies,
            bundled,
            os: string_list(doc.get("os")),
            cpu: string_list(doc.get("cpu")),
            libc: string_list(doc.get("libc")),
            tarball: text(dist.and_then(|d| d.get("tarball")))
                .or_else(|| text(doc.get("resolved"))),
            integrity: text(dist.and_then(|d| d.get("integrity")))
                .or_else(|| text(doc.get("integrity"))),
            shasum: text(dist.and_then(|d| d.get("shasum"))),
            deprecated: doc.get("deprecated").is_some_and(|d| match d {
                Value::String(s) => !s.is_empty(),
                Value::Bool(b) => *b,
                _ => false,
            }),
            has_install_script: doc
                .get("hasInstallScript")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            bin: doc.get("bin").cloned(),
            engines: doc.get("engines").cloned(),
            license: doc.get("license").cloned(),
        }
    }
}

/// A packument: every version of a package, and its dist-tags.
#[derive(Debug)]
pub struct Packument {
    pub dist_tags: BTreeMap<String, String>,
    pub versions: BTreeMap<String, Arc<Manifest>>,
}

impl Packument {
    fn from_json(name: &str, doc: &Value) -> Packument {
        let versions = doc
            .get("versions")
            .and_then(Value::as_object)
            .map(|versions| {
                versions
                    .iter()
                    .map(|(version, doc)| {
                        let mut manifest = Manifest::from_json(doc);
                        if manifest.name.is_empty() {
                            manifest.name = name.to_owned();
                        }
                        manifest.version = version.clone();
                        (version.clone(), Arc::new(manifest))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Packument {
            dist_tags: string_map(doc.get("dist-tags")),
            versions,
        }
    }

    /// The version npm-pick-manifest picks: a tag's version; for a range, the `latest` tag's
    /// when the range admits it, else the highest admitted version, not deprecated if one is.
    pub fn pick(&self, wanted: &Wanted) -> Result<Arc<Manifest>, String> {
        match wanted {
            Wanted::Tag(tag) => {
                let version = self
                    .dist_tags
                    .get(tag)
                    .ok_or_else(|| format!("no dist-tag {tag:?}"))?;
                self.versions.get(version).cloned().ok_or_else(|| {
                    format!("dist-tag {tag:?} names {version}, which is not published")
                })
            }
            Wanted::Range(range) => self.pick_range(range),
        }
    }

    fn pick_range(&self, range: &Range) -> Result<Arc<Manifest>, String> {
        if let Some(latest) = self
            .dist_tags
            .get("latest")
            .and_then(|v| self.versions.get(v))
        {
            let admitted = Version::parse(&latest.version)
                .is_some_and(|v| range.is_any() || range.satisfies(&v));
            if admitted && !latest.deprecated {
                return Ok(Arc::clone(latest));
            }
        }
        self.versions
            .values()
            .filter_map(|m| Version::parse(&m.version).map(|v| (v, m)))
            .filter(|(v, _)| range.satisfies(v))
            .max_by(|(va, a), (vb, b)| (!a.deprecated, va).cmp(&(!b.deprecated, vb)))
            .map(|(_, m)| Arc::clone(m))
            .ok_or_else(|| {
                let mut published: Vec<Version> = self
                    .versions
                    .keys()
                    .filter_map(|v| Version::parse(v))
                    .collect();
                published.sort();
                let shown: Vec<String> = published
                    .iter()
                    .rev()
                    .take(8)
                    .map(|v| v.to_string())
                    .collect();
                format!(
                    "no version matches (published: {})",
                    if shown.is_empty() {
                        "none".to_owned()
                    } else {
                        shown.join(", ")
                    }
                )
            })
    }
}

/// A failed request: the status the server answered, or why there was no answer.
#[derive(Debug)]
pub enum Failure {
    Status(u16),
    Transport(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Status(status) => write!(f, "HTTP {status}"),
            Failure::Transport(message) => f.write_str(message),
        }
    }
}

/// The largest body read: abbreviated packuments of the biggest packages are tens of MB, and
/// no tarball npm accepts is near this.
const MAX_BODY: u64 = 512 * 1024 * 1024;

const PACKUMENT_ACCEPT: &str =
    "application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8, */*";

/// A packument being fetched, or fetched: its first asker fetches, the others wait for it.
type PackumentCell = OnceLock<Result<Arc<Packument>, InstallError>>;

/// The registry as one run sees it: one HTTP agent, packuments fetched once, the tarball cache.
pub struct Registry {
    options: InstallOptions,
    agent: OnceLock<Result<ureq::Agent, String>>,
    packuments: Mutex<HashMap<String, Arc<PackumentCell>>>,
    pub downloaded: AtomicU64,
}

impl Registry {
    pub fn new(options: &InstallOptions) -> Registry {
        Registry {
            options: options.clone(),
            agent: OnceLock::new(),
            packuments: Mutex::new(HashMap::new()),
            downloaded: AtomicU64::new(0),
        }
    }

    /// The registry a package is fetched from: its scope's, else the default.
    fn registry_for(&self, name: &str) -> String {
        let scoped = name
            .split_once('/')
            .filter(|_| name.starts_with('@'))
            .and_then(|(scope, _)| self.options.scoped_registries.get(scope));
        let base = scoped.unwrap_or(&self.options.registry).as_str();
        if base.ends_with('/') {
            base.to_owned()
        } else {
            format!("{base}/")
        }
    }

    /// The packument's URL: a scoped name keeps its `@` and has its `/` escaped, as npm sends it.
    pub fn packument_url(&self, name: &str) -> String {
        format!("{}{}", self.registry_for(name), name.replace('/', "%2f"))
    }

    /// The packument of `name`, fetched at most once per run however many ask for it at once.
    pub fn packument(&self, name: &str) -> Result<Arc<Packument>, InstallError> {
        let cell = {
            let mut map = self.packuments.lock().expect("packument map");
            Arc::clone(map.entry(name.to_owned()).or_default())
        };
        cell.get_or_init(|| self.fetch_packument(name)).clone()
    }

    fn fetch_packument(&self, name: &str) -> Result<Arc<Packument>, InstallError> {
        if self.options.offline {
            return Err(InstallError::Offline {
                package: name.to_owned(),
                needed: "its registry metadata (not in package-lock.json)".to_owned(),
            });
        }
        let url = self.packument_url(name);
        let body = self
            .get(&url, Some(PACKUMENT_ACCEPT))
            .map_err(|failure| match failure {
                Failure::Status(404) => InstallError::NotFound {
                    package: name.to_owned(),
                    url: url.clone(),
                },
                failure => InstallError::Http {
                    package: name.to_owned(),
                    url: url.clone(),
                    reason: failure.to_string(),
                },
            })?;
        let doc: Value = serde_json::from_slice(&body).map_err(|error| InstallError::Http {
            package: name.to_owned(),
            url: url.clone(),
            reason: format!("the packument is not JSON: {error}"),
        })?;
        Ok(Arc::new(Packument::from_json(name, &doc)))
    }

    /// A tarball's bytes: from the cache when it holds the hash expected, else fetched (and then
    /// cached under its SHA-512). The bytes are not yet checked against `integrity`.
    pub fn tarball(
        &self,
        package: &str,
        url: &str,
        integrity: Option<&str>,
    ) -> Result<Vec<u8>, InstallError> {
        if let Some(path) = self.cached_path(integrity)
            && let Ok(bytes) = std::fs::read(&path)
        {
            if integrity::verify(&bytes, integrity, None).is_ok() {
                return Ok(bytes);
            }
            let _ = std::fs::remove_file(&path);
        }
        if self.options.offline {
            return Err(InstallError::Offline {
                package: package.to_owned(),
                needed: format!("its tarball {url} (not in the cache)"),
            });
        }
        self.get(url, None).map_err(|failure| InstallError::Http {
            package: package.to_owned(),
            url: url.to_owned(),
            reason: failure.to_string(),
        })
    }

    fn cached_path(&self, integrity: Option<&str>) -> Option<PathBuf> {
        let key = integrity::sha512_key(integrity?)?;
        Some(content_path(self.options.cache_dir.as_deref()?, &key))
    }

    /// Keeps checked tarball bytes in the cache. A cache that cannot be written is only slower.
    pub fn store(&self, bytes: &[u8]) {
        let Some(dir) = self.options.cache_dir.as_deref() else {
            return;
        };
        let path = content_path(dir, &integrity::sha512_hex(bytes));
        if path.is_file() {
            return;
        }
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let temp = parent.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if std::fs::write(&temp, bytes).is_ok() && std::fs::rename(&temp, &path).is_err() {
            let _ = std::fs::remove_file(&temp);
        }
    }

    fn agent(&self) -> Result<&ureq::Agent, Failure> {
        self.agent
            .get_or_init(|| build_agent(&self.options))
            .as_ref()
            .map_err(|error| Failure::Transport(error.clone()))
    }

    /// The bearer token for `url`: the one given for the registry it is under.
    fn token_for(&self, url: &str) -> Option<&str> {
        self.options
            .tokens
            .iter()
            .filter(|(registry, _)| {
                let prefix = registry.as_str();
                let prefix = prefix.strip_suffix('/').unwrap_or(prefix);
                url.strip_prefix(prefix)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
            .max_by_key(|(registry, _)| registry.as_str().len())
            .map(|(_, token)| token.as_str())
    }

    /// GETs `url`, retrying a server error or a dropped connection with backoff.
    fn get(&self, url: &str, accept: Option<&str>) -> Result<Vec<u8>, Failure> {
        let agent = self.agent()?;
        let mut attempt = 0;
        loop {
            let result = (|| {
                let mut request = agent.get(url);
                if let Some(accept) = accept {
                    request = request.header("Accept", accept);
                }
                if let Some(token) = self.token_for(url) {
                    request = request.header("Authorization", &format!("Bearer {token}"));
                }
                let mut response = request
                    .call()
                    .map_err(|error| Failure::Transport(error.to_string()))?;
                let status = response.status().as_u16();
                if !(200..300).contains(&status) {
                    return Err(Failure::Status(status));
                }
                let mut body = Vec::new();
                response
                    .body_mut()
                    .with_config()
                    .limit(MAX_BODY)
                    .reader()
                    .read_to_end(&mut body)
                    .map_err(|error| Failure::Transport(error.to_string()))?;
                Ok(body)
            })();
            let retry = match &result {
                Err(Failure::Status(status)) => *status >= 500 || *status == 429,
                Err(Failure::Transport(_)) => true,
                Ok(_) => false,
            };
            if !retry || attempt >= self.options.retries {
                if let Ok(body) = &result {
                    self.downloaded
                        .fetch_add(body.len() as u64, Ordering::Relaxed);
                }
                return result;
            }
            std::thread::sleep(Duration::from_millis(200 * 4u64.pow(attempt)));
            attempt += 1;
        }
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn content_path(cache: &Path, sha512_hex: &str) -> PathBuf {
    cache
        .join("content-v1")
        .join("sha512")
        .join(&sha512_hex[..2])
        .join(format!("{sha512_hex}.tgz"))
}

fn build_agent(options: &InstallOptions) -> Result<ureq::Agent, String> {
    use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};
    // The system's trust anchors, as the rest of Verve's TLS uses: a registry behind a
    // corporate CA the system trusts is trusted here too.
    let native = rustls_native_certs::load_native_certs();
    let roots: Vec<Certificate<'static>> = native
        .certs
        .iter()
        .map(|der| Certificate::from_der(der.as_ref()).to_owned())
        .collect();
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::new_with_certs(&roots))
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    let proxy = match &options.proxy {
        Some(proxy) => Some(
            ureq::Proxy::new(proxy.as_str()).map_err(|error| format!("proxy {proxy}: {error}"))?,
        ),
        None => None,
    };
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(options.user_agent.as_str())
        .timeout_global(Some(options.timeout))
        .timeout_connect(Some(options.timeout.min(Duration::from_secs(30))))
        .max_redirects(10)
        .proxy(proxy)
        .tls_config(tls)
        .build();
    Ok(ureq::Agent::new_with_config(config))
}
