//! Installing a `package.json`'s dependencies from an npm registry, with no Node and no npm.
//!
//! A runtime that runs npm packages still needs them on disk, and until now that meant npm —
//! which means Node. This is the install half of `npm install --legacy-peer-deps
//! --ignore-scripts`, in Rust and usable without a JavaScript runtime: [`install`] reads the
//! project's `package.json`, resolves a `node_modules` tree the way npm 7+ lays one out,
//! fetches and checks every tarball, unpacks them, and writes `package-lock.json`. The result is
//! a tree Node's resolver (and Verve's) finds everything in, as it would after npm.
//!
//! What it does as npm does:
//! - **Specs**: node-semver ranges (`^ ~ x * || -`, prerelease rules), exact versions,
//!   dist-tags, `npm:name@range` aliases, tarball URLs. A range takes the `latest` tag's
//!   version when it admits it, else the highest admitted version, avoiding deprecated ones.
//! - **Metadata**: abbreviated packuments, each fetched once per run, from the package's
//!   scope's registry or the default one, with a bearer token where one is given.
//! - **Layout**: hoisted and deduplicated, nested only on conflict (see `tree.rs`).
//! - **Peers**: never installed and never a conflict — `--legacy-peer-deps`. A host that
//!   provides peers itself (as Rebon provides DSH's) wants exactly that.
//! - **Optional**: an optional dependency that cannot be had — not published, no matching
//!   version, a failed download, or `os`/`cpu`/`libc` not this platform's — is skipped with
//!   the reason in the report, along with whatever only it needed. A required package for
//!   another platform fails the install, as `EBADPLATFORM` does.
//! - **Integrity**: `dist.integrity` (else `dist.shasum`) is checked before anything is
//!   unpacked; a mismatch writes nothing.
//! - **Lockfile**: `package-lock.json` (lockfileVersion 3). When it matches `package.json`
//!   and is complete, the tree is installed exactly as locked, with no metadata requests;
//!   otherwise its versions are preferred where they still satisfy.
//! - **Bundled dependencies** come from their bundler's tarball and are never fetched alone.
//!
//! What it does not do, on purpose:
//! - **No scripts**, ever: no `preinstall`, `install`, `postinstall` or `prepare` — npm's
//!   `--ignore-scripts`. A package that needs a native build will not work, and that is the
//!   point: installing must not run the publisher's code.
//! - **No bins**: `node_modules/.bin` is not made. Nothing here runs a package's commands, and
//!   on Windows linking them needs npm's `.cmd` shims.
//! - **No git or local dependencies**: `git+…`, `github:`, `user/repo`, `file:` and `link:`
//!   specs are refused with the reason. A git dependency needs a checkout and usually its
//!   `prepare` script; a local folder is a link.
//! - **No workspaces**, no `overrides`, no audit, no environment or `.npmrc` reading: the
//!   embedder passes registry, tokens and proxy in [`InstallOptions`].
//!
//! Re-running is cheap: a package already in place at the same version, resolution and
//! integrity (by `node_modules/.package-lock.json`, the hidden lockfile npm writes too) is left
//! as it is, and packages the previous install placed that the new tree lacks are removed.
//! Each package is unpacked into a staging folder and renamed into place, so an interrupted
//! install never leaves a half-written package where Node would find it.

mod extract;
mod integrity;
mod registry;
mod resolve;
mod semver;
mod spec;
mod tree;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};

pub use url::Url;

use registry::{Manifest, Registry};
use tree::{EdgeKind, Include, NodeId, ROOT, Tree};

/// Where packages come from and how this install behaves.
#[derive(Clone, Debug)]
pub struct InstallOptions {
    /// The default registry. Defaults to `https://registry.npmjs.org/`; an embedder honouring
    /// `NPM_CONFIG_REGISTRY` reads it and sets it here — nothing is read from the environment.
    pub registry: Url,
    /// Registries by scope (`"@myco"`), like `.npmrc`'s `@myco:registry=`.
    pub scoped_registries: BTreeMap<String, Url>,
    /// Bearer tokens by registry URL: sent with every request under that URL.
    pub tokens: BTreeMap<Url, String>,
    /// A content-addressed tarball cache (by SHA-512). Without one every tarball is fetched.
    pub cache_dir: Option<PathBuf>,
    /// No network: metadata only from `package-lock.json`, tarballs only from the cache.
    pub offline: bool,
    pub user_agent: String,
    /// The most one request may take, body included.
    pub timeout: Duration,
    /// An HTTP proxy (`http://host:port`). No proxy is used unless one is given here.
    pub proxy: Option<String>,
    /// Retries of a request that failed with a 5xx, a 429 or a broken connection.
    pub retries: u32,
    /// Requests in flight at once.
    pub concurrency: usize,
    /// Install the project's `devDependencies` too. Off: a plugin's dev tree is not what runs.
    pub include_dev: bool,
    /// Install `optionalDependencies` (`--omit=optional` when off).
    pub include_optional: bool,
    /// Install from `package-lock.json` when it matches `package.json`.
    pub use_lockfile: bool,
    /// Write `package-lock.json` (`--no-package-lock` when off).
    pub write_lockfile: bool,
    /// The platform `os`, `cpu` and `libc` fields are checked against.
    pub platform: Platform,
}

impl Default for InstallOptions {
    fn default() -> Self {
        InstallOptions {
            registry: Url::parse("https://registry.npmjs.org/").expect("a literal URL"),
            scoped_registries: BTreeMap::new(),
            tokens: BTreeMap::new(),
            cache_dir: None,
            offline: false,
            user_agent: format!(
                "verve/{} (npm-compatible installer)",
                env!("CARGO_PKG_VERSION")
            ),
            timeout: Duration::from_secs(300),
            proxy: None,
            retries: 2,
            concurrency: 8,
            include_dev: false,
            include_optional: true,
            use_lockfile: true,
            write_lockfile: true,
            platform: Platform::current(),
        }
    }
}

/// A platform by Node's names: `process.platform`, `process.arch`, and on Linux the libc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub cpu: String,
    pub libc: Option<String>,
}

impl Platform {
    /// The platform this process runs on.
    pub fn current() -> Platform {
        let os = match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            other => other,
        };
        let cpu = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "x86" => "ia32",
            "aarch64" => "arm64",
            "powerpc64" => "ppc64",
            "loongarch64" => "loong64",
            other => other,
        };
        let libc = (os == "linux").then(|| {
            if cfg!(target_env = "musl") {
                "musl"
            } else {
                "glibc"
            }
            .to_owned()
        });
        Platform {
            os: os.to_owned(),
            cpu: cpu.to_owned(),
            libc,
        }
    }

    /// npm-install-checks' rule: none of the `!`-negated values, and one of the others if any.
    fn allows(value: Option<&str>, list: &[String]) -> bool {
        if list.is_empty() || (list.len() == 1 && list[0] == "any") {
            return true;
        }
        let Some(value) = value else { return false };
        let mut negated = 0;
        let mut matched = false;
        for entry in list {
            match entry.strip_prefix('!') {
                Some(not) => {
                    negated += 1;
                    if not == value {
                        return false;
                    }
                }
                None => matched |= entry == value,
            }
        }
        matched || negated == list.len()
    }

    fn check(&self, manifest: &Manifest) -> Result<(), String> {
        let checks = [
            ("os", Some(self.os.as_str()), &manifest.os),
            ("cpu", Some(self.cpu.as_str()), &manifest.cpu),
            ("libc", self.libc.as_deref(), &manifest.libc),
        ];
        for (field, value, list) in checks {
            if !Platform::allows(value, list) {
                return Err(format!(
                    "it is for {field} {} and this is {}",
                    list.join(", "),
                    value.unwrap_or("none")
                ));
            }
        }
        Ok(())
    }
}

/// Why an install failed. Every one names the package it is about.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum InstallError {
    /// The project's `package.json` (or `package-lock.json`) could not be read.
    Project {
        path: PathBuf,
        reason: String,
    },
    /// A dependency spec this installer does not install (git, local paths) or cannot read.
    Spec {
        package: String,
        spec: String,
        reason: String,
    },
    /// The registry has no such package (404).
    NotFound {
        package: String,
        url: String,
    },
    /// The package has no version the spec admits.
    NoMatchingVersion {
        package: String,
        spec: String,
        reason: String,
    },
    /// A request failed after its retries.
    Http {
        package: String,
        url: String,
        reason: String,
    },
    /// Offline, and what was needed is neither locked nor cached.
    Offline {
        package: String,
        needed: String,
    },
    /// The tarball is not the one the registry vouched for. Nothing of it was written.
    Integrity {
        package: String,
        version: String,
        expected: String,
        actual: String,
    },
    /// The tarball holds an entry that would land outside its folder. Nothing of it is kept.
    UnsafeTarball {
        package: String,
        version: String,
        entry: String,
        reason: String,
    },
    /// The tarball is not a gzipped tar, or lacks what a package has.
    BadTarball {
        package: String,
        version: String,
        reason: String,
    },
    /// A required package is for another `os`, `cpu` or `libc`.
    BadPlatform {
        package: String,
        version: String,
        reason: String,
    },
    /// No folder from the dependent up could take the package.
    Conflict {
        package: String,
        location: String,
    },
    Io {
        path: PathBuf,
        reason: String,
    },
    /// A dependency could not be installed; `error` says why.
    Dependency {
        name: String,
        spec: String,
        required_by: String,
        error: Box<InstallError>,
    },
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::Project { path, reason } => write!(f, "{}: {reason}", path.display()),
            InstallError::Spec {
                package,
                spec,
                reason,
            } => {
                write!(f, "{package}@{spec} cannot be installed: {reason}")
            }
            InstallError::NotFound { package, url } => {
                write!(f, "{package} is not in the registry (404 from {url})")
            }
            InstallError::NoMatchingVersion {
                package,
                spec,
                reason,
            } => {
                write!(f, "{package}@{spec}: {reason}")
            }
            InstallError::Http {
                package,
                url,
                reason,
            } => {
                write!(f, "{package}: fetching {url} failed: {reason}")
            }
            InstallError::Offline { package, needed } => {
                write!(f, "{package}: offline, and {needed}")
            }
            InstallError::Integrity {
                package,
                version,
                expected,
                actual,
            } => write!(
                f,
                "{package}@{version}: the tarball's integrity check failed (expected {expected}, got {actual})"
            ),
            InstallError::UnsafeTarball {
                package,
                version,
                entry,
                reason,
            } => write!(
                f,
                "{package}@{version}: the tarball's entry {entry:?} {reason}; refusing the package"
            ),
            InstallError::BadTarball {
                package,
                version,
                reason,
            } => {
                write!(f, "{package}@{version}: the tarball is unusable: {reason}")
            }
            InstallError::BadPlatform {
                package,
                version,
                reason,
            } => {
                write!(f, "{package}@{version} is not for this platform: {reason}")
            }
            InstallError::Conflict { package, location } => write!(
                f,
                "{package}: no folder from {} up can hold it without a conflict",
                resolve::shown_location(location)
            ),
            InstallError::Io { path, reason } => write!(f, "{}: {reason}", path.display()),
            InstallError::Dependency {
                required_by, error, ..
            } => {
                write!(f, "{error} (required by {required_by})")
            }
        }
    }
}

impl std::error::Error for InstallError {}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> InstallError + '_ {
    move |error| InstallError::Io {
        path: path.to_owned(),
        reason: error.to_string(),
    }
}

/// One package in `node_modules`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPackage {
    /// Where, relative to the project: `node_modules/a/node_modules/b`.
    pub location: String,
    /// Its real name (an alias's folder is named otherwise; see `location`).
    pub name: String,
    pub version: String,
    pub resolved: Option<String>,
    pub integrity: Option<String>,
}

/// A package not installed, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedPackage {
    pub name: String,
    /// The spec asked for, or the version resolved when it was resolved.
    pub wanted: String,
    /// Where it would have been, when it was placed.
    pub location: Option<String>,
    pub required_by: String,
    pub reason: String,
}

/// What [`install`] did.
#[derive(Clone, Debug, Default)]
pub struct InstallReport {
    /// Unpacked by this run.
    pub installed: Vec<InstalledPackage>,
    /// Already in place, left as they were.
    pub unchanged: Vec<InstalledPackage>,
    /// Optional packages left out, with the reason.
    pub skipped: Vec<SkippedPackage>,
    /// Folders a previous install placed that this tree does not have, removed.
    pub removed: Vec<String>,
    /// Bytes received over the network: metadata and tarballs.
    pub downloaded_bytes: u64,
    /// The size of the tarballs unpacked by this run.
    pub tarball_bytes: u64,
    /// The size of the files they unpacked to.
    pub unpacked_bytes: u64,
    /// Whether the tree came from `package-lock.json` as it was.
    pub from_lockfile: bool,
    /// The lockfile written, when one was (it is not rewritten unchanged).
    pub lockfile: Option<PathBuf>,
}

/// Maps `items` on up to `threads` threads, answering in the items' order.
pub(crate) fn parallel_map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    if items.len() <= 1 || threads <= 1 {
        return items.iter().map(work).collect();
    }
    let next = AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<R>>> =
        items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..threads.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let result = work(item);
                    *results[i].lock().expect("result slot") = Some(result);
                }
            });
        }
    });
    results
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("result slot")
                .expect("every item is mapped")
        })
        .collect()
}

fn read_json(path: &Path) -> Result<Option<Value>, InstallError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(path)(error)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| InstallError::Project {
            path: path.to_owned(),
            reason: format!("not JSON: {error}"),
        })
}

fn string_map(doc: &Value, key: &str) -> BTreeMap<String, String> {
    doc.get(key)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// Installs the dependencies of the `package.json` in `project_dir` into its `node_modules`.
pub fn install(
    project_dir: &Path,
    options: &InstallOptions,
) -> Result<InstallReport, InstallError> {
    let package_path = project_dir.join("package.json");
    let package = read_json(&package_path)?.ok_or_else(|| InstallError::Project {
        path: package_path.clone(),
        reason: "there is no package.json".to_owned(),
    })?;
    if !package.is_object() {
        return Err(InstallError::Project {
            path: package_path,
            reason: "package.json is not an object".to_owned(),
        });
    }
    let mut project = Manifest::from_json(&package);
    // The project is not a package of the tree: what it bundles is its own business.
    project.bundled.clear();
    let project = Arc::new(project);
    let dev = string_map(&package, "devDependencies");
    let include = Include {
        dev: options.include_dev,
        optional: options.include_optional,
    };
    let root_edges = tree::edges_of(&project, true, include, &dev);
    let registry = Registry::new(options);

    let lock_path = project_dir.join("package-lock.json");
    let lock = if options.use_lockfile {
        read_json(&lock_path)?
    } else {
        None
    };
    let lock_packages = lock
        .as_ref()
        .filter(|lock| {
            lock.get("lockfileVersion")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                >= 2
        })
        .and_then(|lock| lock.get("packages"))
        .and_then(Value::as_object);

    let mut from_lockfile = false;
    let mut locked = None;
    let mut hints: HashMap<String, Vec<Arc<Manifest>>> = HashMap::new();
    if let Some(packages) = lock_packages {
        let (tree, complete) =
            Tree::from_lock(Arc::clone(&project), root_edges.clone(), packages, include);
        let lock_root = packages.get("").cloned().unwrap_or(Value::Null);
        let root_matches = string_map(&lock_root, "dependencies") == project.dependencies
            && (!include.optional
                || string_map(&lock_root, "optionalDependencies") == project.optional_dependencies)
            && (!include.dev || string_map(&lock_root, "devDependencies") == dev);
        if complete && root_matches {
            from_lockfile = true;
            locked = Some(tree);
        } else {
            for id in tree.live() {
                let node = &tree.nodes[id];
                if !node.in_bundle && node.manifest.tarball.is_some() {
                    hints
                        .entry(node.manifest.name.clone())
                        .or_default()
                        .push(Arc::clone(&node.manifest));
                }
            }
        }
    }

    let mut resolver =
        resolve::Resolver::new(&registry, options.concurrency.max(1), include, hints);
    let tree = match locked {
        Some(mut tree) => {
            tree.prune();
            tree
        }
        None => {
            let mut tree = Tree::new(Arc::clone(&project), root_edges);
            resolver.build(&mut tree);
            tree
        }
    };
    let plan = resolver.finish(tree, &options.platform)?;

    let mut report = reify(project_dir, &plan, &registry, options)?;
    report.from_lockfile = from_lockfile;
    report.skipped.extend(plan.skipped.iter().cloned());
    report.downloaded_bytes = registry.downloaded.load(Ordering::Relaxed);

    let (lock, hidden) = lockfiles(project_dir, &package, &dev, &plan, include, &report);
    let hidden_path = project_dir.join("node_modules").join(".package-lock.json");
    if report.installed.is_empty() && report.unchanged.is_empty() {
        if hidden_path.exists() {
            std::fs::remove_file(&hidden_path).map_err(io_error(&hidden_path))?;
        }
    } else {
        let text = format!("{}\n", serde_json::to_string_pretty(&hidden).expect("JSON"));
        write_atomic(&hidden_path, text.as_bytes())?;
    }
    if options.write_lockfile {
        let text = format!("{}\n", serde_json::to_string_pretty(&lock).expect("JSON"));
        let current = std::fs::read_to_string(&lock_path).ok();
        if current.as_deref() != Some(text.as_str()) {
            write_atomic(&lock_path, text.as_bytes())?;
            report.lockfile = Some(lock_path);
        }
    }
    Ok(report)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, bytes).map_err(io_error(&temp))?;
    rename_retrying(&temp, path).map_err(io_error(path))
}

/// A rename, retried briefly: on Windows a scanner holding a just-written file open makes the
/// first attempt fail with "access denied".
fn rename_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(error) if attempt < 5 && error.kind() == std::io::ErrorKind::PermissionDenied => {
                std::thread::sleep(Duration::from_millis(50 << attempt));
                attempt += 1;
            }
            result => return result,
        }
    }
}

fn remove_dir(path: &Path) -> Result<(), InstallError> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(io_error(path)(error)),
        _ => Ok(()),
    }
}

fn package_dir(project_dir: &Path, location: &str) -> PathBuf {
    location
        .split('/')
        .fold(project_dir.to_owned(), |path, part| path.join(part))
}

/// What a tarball turned into: the integrity checked, and its sizes.
struct Unpacked {
    integrity: String,
    tarball: u64,
    files: u64,
}

fn fetch_and_unpack(
    plan: &resolve::Plan,
    registry: &Registry,
    id: NodeId,
    staging: &Path,
) -> Result<Unpacked, InstallError> {
    let manifest = &plan.tree.nodes[id].manifest;
    let (name, version) = (manifest.name.clone(), manifest.version.clone());
    let url = manifest
        .tarball
        .as_deref()
        .ok_or_else(|| InstallError::BadTarball {
            package: name.clone(),
            version: version.clone(),
            reason: "no tarball URL".to_owned(),
        })?;
    let bytes = match plan.fetched.get(url) {
        Some(bytes) => Arc::clone(bytes),
        None => Arc::new(registry.tarball(&name, url, manifest.integrity.as_deref())?),
    };
    let integrity = integrity::verify(
        &bytes,
        manifest.integrity.as_deref(),
        manifest.shasum.as_deref(),
    )
    .map_err(|mismatch| InstallError::Integrity {
        package: name.clone(),
        version: version.clone(),
        expected: mismatch.expected,
        actual: mismatch.actual,
    })?;
    registry.store(&bytes);
    let files = extract::extract(&bytes, staging).map_err(|error| match error {
        extract::ExtractError::Unsafe { entry, reason } => InstallError::UnsafeTarball {
            package: name.clone(),
            version: version.clone(),
            entry,
            reason,
        },
        extract::ExtractError::Corrupt(reason) => InstallError::BadTarball {
            package: name.clone(),
            version: version.clone(),
            reason,
        },
        extract::ExtractError::Io { path, error } => io_error(&path)(error),
    })?;
    Ok(Unpacked {
        integrity,
        tarball: bytes.len() as u64,
        files,
    })
}

fn installed_package(node: &tree::Node, integrity: Option<String>) -> InstalledPackage {
    InstalledPackage {
        location: node.location.clone(),
        name: node.manifest.name.clone(),
        version: node.manifest.version.clone(),
        resolved: node.manifest.tarball.clone(),
        integrity,
    }
}

/// Puts the plan's packages on disk.
fn reify(
    project_dir: &Path,
    plan: &resolve::Plan,
    registry: &Registry,
    options: &InstallOptions,
) -> Result<InstallReport, InstallError> {
    let tree = &plan.tree;
    let node_modules = project_dir.join("node_modules");
    let hidden_path = node_modules.join(".package-lock.json");
    let previous: Map<String, Value> = read_json(&hidden_path)
        .ok()
        .flatten()
        .and_then(|doc| doc.get("packages").and_then(Value::as_object).cloned())
        .unwrap_or_default();

    let mut wanted: Vec<NodeId> = tree
        .live()
        .filter(|id| !plan.skip.contains(id) && !tree.nodes[*id].in_bundle)
        .collect();
    wanted.sort_by_key(|&id| (tree.nodes[id].depth(), tree.nodes[id].location.clone()));

    // In place already: same version, tarball and integrity as recorded, under a parent that
    // stays in place too (replacing a folder replaces what is nested in it).
    let mut current: HashSet<NodeId> = HashSet::new();
    for &id in &wanted {
        let node = &tree.nodes[id];
        let Some(before) = previous.get(&node.location) else {
            continue;
        };
        let text = |key: &str| before.get(key).and_then(Value::as_str);
        let same = text("version") == Some(node.manifest.version.as_str())
            && text("resolved") == node.manifest.tarball.as_deref()
            && match (text("integrity"), node.manifest.integrity.as_deref()) {
                // Recorded as checked, which for an old package is a SHA-512 the registry
                // never gave: only hashes in the same algorithm compare.
                (Some(a), Some(b)) if a.split('-').next() == b.split('-').next() => a == b,
                _ => true,
            };
        let parent_stays = node
            .parent
            .is_none_or(|p| p == ROOT || current.contains(&p));
        let on_disk = read_json(&package_dir(project_dir, &node.location).join("package.json"))
            .ok()
            .flatten()
            .is_some_and(|pkg| {
                pkg.get("version").and_then(Value::as_str) == Some(node.manifest.version.as_str())
            });
        if same && parent_stays && on_disk {
            current.insert(id);
        }
    }

    let mut report = InstallReport::default();
    // What the previous install placed and this tree does not have goes first, so nothing
    // stale shadows a hoisted package.
    let keep: HashSet<&str> = wanted
        .iter()
        .map(|&id| tree.nodes[id].location.as_str())
        .collect();
    let mut stale: Vec<&String> = previous
        .iter()
        .filter(|(location, entry)| {
            !location.is_empty()
                && entry.get("inBundle").and_then(Value::as_bool) != Some(true)
                && !keep.contains(location.as_str())
        })
        .map(|(location, _)| location)
        .collect();
    stale.sort();
    for location in stale {
        let dir = package_dir(project_dir, location);
        if dir.exists() {
            remove_dir(&dir)?;
            report.removed.push(location.clone());
        }
    }

    let work: Vec<NodeId> = wanted
        .iter()
        .copied()
        .filter(|id| !current.contains(id))
        .collect();
    let staging = node_modules.join(".verve-staging");
    remove_dir(&staging)?;
    if !work.is_empty() {
        std::fs::create_dir_all(&staging).map_err(io_error(&staging))?;
    }
    let results = parallel_map(&work, options.concurrency.max(1), |&id| {
        fetch_and_unpack(plan, registry, id, &staging.join(id.to_string()))
    });

    // A failed optional package is left out with what only it needed; a failed required one
    // fails the install before anything is moved into place.
    let required = tree.reachable(|edge| edge.kind != EdgeKind::Optional, &HashSet::new());
    let mut failed: HashSet<NodeId> = HashSet::new();
    let mut unpacked: HashMap<NodeId, Unpacked> = HashMap::new();
    for (&id, result) in work.iter().zip(results) {
        match result {
            Ok(done) => {
                unpacked.insert(id, done);
            }
            Err(error) if !required[id] => {
                let node = &tree.nodes[id];
                report.skipped.push(SkippedPackage {
                    name: node.manifest.name.clone(),
                    wanted: node.manifest.version.clone(),
                    location: Some(node.location.clone()),
                    required_by: String::new(),
                    reason: error.to_string(),
                });
                failed.insert(id);
            }
            Err(error) => {
                let _ = remove_dir(&staging);
                return Err(error);
            }
        }
    }
    let mut excluded: HashSet<NodeId> = plan.skip.clone();
    excluded.extend(failed.iter().copied());
    let reached = tree.reachable(|_| true, &excluded);

    for &id in &wanted {
        let node = &tree.nodes[id];
        let dir = package_dir(project_dir, &node.location);
        if !reached[id] {
            if dir.exists() && previous.contains_key(&node.location) {
                remove_dir(&dir)?;
                report.removed.push(node.location.clone());
            }
            continue;
        }
        if current.contains(&id) {
            let integrity = node.manifest.integrity.clone().or_else(|| {
                previous
                    .get(&node.location)
                    .and_then(|e| e.get("integrity"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            report.unchanged.push(installed_package(node, integrity));
            continue;
        }
        let Some(done) = unpacked.get(&id) else {
            continue;
        };
        remove_dir(&dir)?;
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(io_error(parent))?;
        }
        rename_retrying(&staging.join(id.to_string()), &dir).map_err(io_error(&dir))?;
        report.tarball_bytes += done.tarball;
        report.unpacked_bytes += done.files;
        report
            .installed
            .push(installed_package(node, Some(done.integrity.clone())));
    }
    remove_dir(&staging)?;
    Ok(report)
}

/// A lockfile entry for one package.
fn entry_of(manifest: &Manifest, folder_name: &str, integrity: Option<&str>) -> Map<String, Value> {
    let mut entry = Map::new();
    if manifest.name != folder_name {
        entry.insert("name".into(), json!(manifest.name));
    }
    entry.insert("version".into(), json!(manifest.version));
    if let Some(resolved) = &manifest.tarball {
        entry.insert("resolved".into(), json!(resolved));
    }
    if let Some(integrity) = integrity.or(manifest.integrity.as_deref()) {
        entry.insert("integrity".into(), json!(integrity));
    }
    for (key, map) in [
        ("dependencies", &manifest.dependencies),
        ("optionalDependencies", &manifest.optional_dependencies),
        ("peerDependencies", &manifest.peer_dependencies),
    ] {
        if !map.is_empty() {
            entry.insert(key.into(), json!(map));
        }
    }
    if let Some(meta) = &manifest.peer_dependencies_meta {
        entry.insert("peerDependenciesMeta".into(), meta.clone());
    }
    for (key, list) in [
        ("bundleDependencies", &manifest.bundled),
        ("os", &manifest.os),
        ("cpu", &manifest.cpu),
        ("libc", &manifest.libc),
    ] {
        if !list.is_empty() {
            entry.insert(key.into(), json!(list));
        }
    }
    if manifest.has_install_script {
        entry.insert("hasInstallScript".into(), json!(true));
    }
    for (key, value) in [
        ("bin", &manifest.bin),
        ("engines", &manifest.engines),
        ("license", &manifest.license),
    ] {
        if let Some(value) = value {
            entry.insert(key.into(), value.clone());
        }
    }
    entry
}

/// The packages a bundler's tarball brought, read from where it unpacked them.
fn bundled_entries(dir: &Path, location: &str, out: &mut BTreeMap<String, Value>) {
    let Ok(entries) = std::fs::read_dir(dir.join("node_modules")) else {
        return;
    };
    let mut names: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if name.starts_with('@') {
            if let Ok(scoped) = std::fs::read_dir(entry.path()) {
                for inner in scoped.flatten() {
                    let inner_name = inner.file_name().to_string_lossy().into_owned();
                    names.push((format!("{name}/{inner_name}"), inner.path()));
                }
            }
        } else {
            names.push((name, entry.path()));
        }
    }
    for (name, path) in names {
        let Ok(Some(doc)) = read_json(&path.join("package.json")) else {
            continue;
        };
        let mut manifest = Manifest::from_json(&doc);
        if manifest.name.is_empty() {
            manifest.name = name.clone();
        }
        manifest.tarball = None;
        manifest.integrity = None;
        let child = tree::child_location(location, &name);
        let mut entry = entry_of(&manifest, &name, None);
        entry.insert("inBundle".into(), json!(true));
        out.insert(child.clone(), Value::Object(entry));
        bundled_entries(&path, &child, out);
    }
}

/// npm's `dev`, `optional` and `devOptional` flags: what only the dev tree, or only optional
/// edges, reach.
fn flags(tree: &Tree) -> Vec<(bool, bool, bool)> {
    let none = HashSet::new();
    let from_root =
        |follow: &dyn Fn(&tree::Edge) -> bool| tree.reachable(|edge| follow(edge), &none);
    let prod_any = from_root(&|edge| edge.kind != EdgeKind::Dev);
    let non_optional = from_root(&|edge| edge.kind != EdgeKind::Optional);
    let prod_required = from_root(&|edge| edge.kind == EdgeKind::Prod);
    (0..tree.nodes.len())
        .map(|id| {
            let dev = !prod_any[id];
            let optional = !non_optional[id];
            let dev_optional = !dev && !optional && !prod_required[id];
            (dev, optional, dev_optional)
        })
        .collect()
}

/// The lockfile's `packages` (the project's entry aside), and which of them are on disk.
fn lockfile_packages(
    project_dir: &Path,
    tree: &Tree,
    report: &InstallReport,
    include: Include,
) -> (BTreeMap<String, Value>, HashSet<String>) {
    let integrity: HashMap<&str, &str> = report
        .installed
        .iter()
        .chain(&report.unchanged)
        .filter_map(|p| p.integrity.as_deref().map(|i| (p.location.as_str(), i)))
        .collect();
    let mut on_disk: HashSet<String> = report
        .installed
        .iter()
        .chain(&report.unchanged)
        .map(|p| p.location.clone())
        .collect();
    let mut packages = BTreeMap::new();
    for id in tree.live() {
        let node = &tree.nodes[id];
        if node.in_bundle {
            // A bundler on disk is read for its bundle below; one not installed keeps what a
            // lock said of its bundle, and a fresh placeholder says nothing.
            let bundler_on_disk = node
                .parent
                .is_some_and(|p| on_disk.contains(&tree.nodes[p].location));
            if !bundler_on_disk && !node.manifest.version.is_empty() {
                let mut entry = entry_of(&node.manifest, &node.name, None);
                entry.insert("inBundle".into(), json!(true));
                packages.insert(node.location.clone(), Value::Object(entry));
            }
            continue;
        }
        let entry = entry_of(
            &node.manifest,
            &node.name,
            integrity.get(node.location.as_str()).copied(),
        );
        packages.insert(node.location.clone(), Value::Object(entry));
        if !node.manifest.bundled.is_empty() && on_disk.contains(&node.location) {
            let mut bundle = BTreeMap::new();
            bundled_entries(
                &package_dir(project_dir, &node.location),
                &node.location,
                &mut bundle,
            );
            on_disk.extend(bundle.keys().cloned());
            packages.extend(bundle);
        }
    }
    // The flags are the lockfile's own: read back as a tree, so a bundle's optional-only
    // packages are marked as npm marks them.
    let mut root_edges = tree.nodes[ROOT].edges.clone();
    for edge in &mut root_edges {
        edge.to = None;
    }
    let map: Map<String, Value> = packages.clone().into_iter().collect();
    let (read_back, _) = Tree::from_lock(
        Arc::clone(&tree.nodes[ROOT].manifest),
        root_edges,
        &map,
        include,
    );
    let flags = flags(&read_back);
    for id in read_back.live() {
        let node = &read_back.nodes[id];
        let (dev, optional, dev_optional) = flags[id];
        if let Some(Value::Object(entry)) = packages.get_mut(&node.location) {
            for (key, set) in [
                ("dev", dev),
                ("optional", optional),
                ("devOptional", dev_optional),
            ] {
                if set {
                    entry.insert(key.into(), json!(true));
                }
            }
        }
    }
    (packages, on_disk)
}

/// `package-lock.json`, and `node_modules/.package-lock.json` — the same entries, only those
/// on disk.
fn lockfiles(
    project_dir: &Path,
    package: &Value,
    dev: &BTreeMap<String, String>,
    plan: &resolve::Plan,
    include: Include,
    report: &InstallReport,
) -> (Value, Value) {
    let project = &plan.tree.nodes[ROOT].manifest;
    let (entries, on_disk) = lockfile_packages(project_dir, &plan.tree, report, include);
    let mut root = Map::new();
    for key in ["name", "version"] {
        if let Some(value) = package.get(key) {
            root.insert(key.into(), value.clone());
        }
    }
    let mut kinds = vec![("dependencies", &project.dependencies)];
    if include.optional {
        kinds.push(("optionalDependencies", &project.optional_dependencies));
    }
    if include.dev {
        kinds.push(("devDependencies", dev));
    }
    for (key, map) in kinds {
        if !map.is_empty() {
            root.insert(key.into(), json!(map));
        }
    }
    let shell = |packages: Map<String, Value>| {
        let mut lock = Map::new();
        for key in ["name", "version"] {
            if let Some(value) = package.get(key) {
                lock.insert(key.into(), value.clone());
            }
        }
        lock.insert("lockfileVersion".into(), json!(3));
        lock.insert("requires".into(), json!(true));
        lock.insert("packages".into(), Value::Object(packages));
        Value::Object(lock)
    };
    let hidden: Map<String, Value> = entries
        .iter()
        .filter(|(location, _)| on_disk.contains(*location))
        .map(|(location, entry)| (location.clone(), entry.clone()))
        .collect();
    let mut full = Map::new();
    full.insert(String::new(), Value::Object(root));
    full.extend(entries);
    (shell(full), shell(hidden))
}

/// One package's published tarball, as `npm pack <spec>` leaves it.
#[derive(Clone, Debug)]
pub struct Packed {
    pub name: String,
    pub version: String,
    /// The `.tgz` written, named as npm names it (`scope-name-1.2.3.tgz`).
    pub tarball: PathBuf,
    /// The integrity it was checked against, or its `sha512` when the registry vouched for none.
    pub integrity: String,
}

/// Fetches the tarball `spec` names into `dest`, as `npm pack <spec> --ignore-scripts` does:
/// `name` (its `latest`), `name@range`, `name@tag`, or a tarball URL. It is checked against the
/// integrity the registry published and written as it came — never unpacked, no script run —
/// for whoever asked to read it their own way.
pub fn pack(spec: &str, options: &InstallOptions, dest: &Path) -> Result<Packed, InstallError> {
    let registry = Registry::new(options);
    let (name, wanted) = split_pack_spec(spec);
    let parsed = if name.is_empty() {
        spec::Spec::Tarball {
            url: wanted.to_owned(),
        }
    } else {
        spec::parse(name, wanted).map_err(|error| InstallError::Spec {
            package: name.to_owned(),
            spec: wanted.to_owned(),
            reason: error.to_string(),
        })?
    };
    let (manifest, bytes) = match &parsed {
        spec::Spec::Registry { name, wanted } => {
            let packument = registry.packument(name)?;
            let manifest =
                packument
                    .pick(wanted)
                    .map_err(|reason| InstallError::NoMatchingVersion {
                        package: name.clone(),
                        spec: spec.to_owned(),
                        reason,
                    })?;
            let url = manifest
                .tarball
                .clone()
                .ok_or_else(|| InstallError::BadTarball {
                    package: manifest.name.clone(),
                    version: manifest.version.clone(),
                    reason: "no tarball URL".to_owned(),
                })?;
            let bytes = registry.tarball(&manifest.name, &url, manifest.integrity.as_deref())?;
            (manifest, bytes)
        }
        spec::Spec::Tarball { url } => {
            let bytes = registry.tarball(name, url, None)?;
            let doc =
                extract::read_package_json(&bytes).map_err(|reason| InstallError::BadTarball {
                    package: name.to_owned(),
                    version: url.clone(),
                    reason,
                })?;
            (Arc::new(Manifest::from_json(&doc)), bytes)
        }
    };
    let integrity = integrity::verify(
        &bytes,
        manifest.integrity.as_deref(),
        manifest.shasum.as_deref(),
    )
    .map_err(|mismatch| InstallError::Integrity {
        package: manifest.name.clone(),
        version: manifest.version.clone(),
        expected: mismatch.expected,
        actual: mismatch.actual,
    })?;
    registry.store(&bytes);
    let file = format!(
        "{}-{}.tgz",
        manifest.name.trim_start_matches('@').replace('/', "-"),
        manifest.version
    );
    std::fs::create_dir_all(dest).map_err(io_error(dest))?;
    let tarball = dest.join(file);
    std::fs::write(&tarball, &bytes).map_err(io_error(&tarball))?;
    Ok(Packed {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        tarball,
        integrity,
    })
}

/// `name@wanted` apart, the way npm reads a pack argument: the `@` of a scope is part of the
/// name, a bare name wants `latest`, and a URL is a tarball of its own.
fn split_pack_spec(spec: &str) -> (&str, &str) {
    if spec.starts_with("https://") || spec.starts_with("http://") {
        return ("", spec);
    }
    match spec
        .get(1..)
        .and_then(|rest| rest.find('@'))
        .map(|at| at + 1)
    {
        Some(at) => (&spec[..at], &spec[at + 1..]),
        None => (spec, "latest"),
    }
}
