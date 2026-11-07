//! `electron <args>`: what Electron 39's command line means, and where a packaged app lives.
//!
//! The rules are those of Electron's `default_app/main.ts` and of the Chromium command line it
//! sits on: a switch (`--name[=value]`) is kept, in `process.argv` and in `app.commandLine`,
//! whether or not anything here knows it; the first argument that is not a switch is the app;
//! and an executable with `resources/app.asar` (or `resources/app/`) beside it is a packaged
//! app, which takes every argument as its own. Nothing here touches a window or a thread, so
//! each rule has a test.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use serde_json::Value;
use verve_asar::{AsarOptions, AsarOverlay};

use crate::ElectronOptions;
use crate::hub::{Hub, Program};

/// The environment a launch is decided in: `std::env::vars` for a real one, a list for a test.
#[derive(Clone, Debug, Default)]
pub struct Env(BTreeMap<String, String>);

impl Env {
    /// This process's environment.
    pub fn from_process() -> Self {
        Self(std::env::vars().collect())
    }

    /// The same, with `key` set to `value`.
    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.0.insert(key.into(), value.into());
        self
    }

    /// A variable's value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Whether `key` is on: Electron's `IsEnvSet` reads any value but the empty one as set, so
    /// `ELECTRON_RUN_AS_NODE=0` runs as Node (observed on 39.8.10).
    pub fn is_on(&self, key: &str) -> bool {
        self.get(key).is_some_and(|value| !value.is_empty())
    }
}

/// A Chromium switch: `--name` has an empty value, `--name=value` has `value`.
pub type Switch = (String, String);

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    /// `-v`: print the version and leave.
    Version,
    /// `-a`: print the module ABI and leave.
    Abi,
    /// `-h`: print the usage and leave.
    Help,
    /// `-i`: there is no REPL to open.
    Interactive,
    /// `ELECTRON_RUN_AS_NODE`: this executable is Node, and these are Node's arguments.
    RunAsNode { args: Vec<OsString> },
    /// An app to run.
    Start(Box<Start>),
}

/// An app to run, and the command line it was started with.
#[derive(Debug, PartialEq, Eq)]
pub struct Start {
    /// What to run.
    pub target: Target,
    /// `process.argv` after the executable: every argument, as it was given.
    pub argv: Vec<String>,
    /// The switches among them, in order, for `app.commandLine`.
    pub switches: Vec<Switch>,
    /// Modules `-r` asked to load before the app's entry.
    pub requires: Vec<String>,
    /// `process.resourcesPath`.
    pub resources_path: PathBuf,
    /// The `package.json` of the app: only a folder or a packaged app has one that counts. An
    /// app given as a script is named "Electron", whatever sits beside it.
    pub manifest: Manifest,
}

impl Start {
    /// A development app in `app_path`: what an app is when nothing said how it was started, as
    /// in a test. Its profile directory is in the temporary one, so an app that is only being
    /// tried never makes a folder among the user's.
    pub fn development(app_path: PathBuf) -> Self {
        Self {
            argv: vec![app_path.display().to_string()],
            target: Target::App {
                app_path,
                entry: PathBuf::new(),
            },
            switches: vec![(
                "user-data-dir".into(),
                scratch_profile().display().to_string(),
            )],
            requires: Vec::new(),
            resources_path: packaged_in(&exe_path()).0,
            manifest: Manifest::default(),
        }
    }
}

/// A profile directory in the temporary one, of this call's own: the profile of an app nobody
/// launched, which its Hub removes as the app ends.
fn scratch_profile() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("verve-electron-start-{}-{n}", std::process::id()))
}

impl Start {
    /// The profile directory the command line names with `--user-data-dir`, if it does.
    pub fn user_data_dir(&self) -> Option<PathBuf> {
        let (_, value) = self
            .switches
            .iter()
            .rev()
            .find(|(n, _)| n == "user-data-dir")?;
        Some(std::path::absolute(value).unwrap_or_else(|_| PathBuf::from(value)))
    }
}

/// This executable.
pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("electron"))
}

/// The app a launch runs.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// `electron <dir | file.js>`: development, `app_path` being what `app.getAppPath()` says.
    App { app_path: PathBuf, entry: PathBuf },
    /// An app beside the executable: `app_path` is its `app.asar` or `app` folder.
    Packaged { app_path: PathBuf },
    /// `electron <url | file.html>`: the default app, opening a window on it.
    OpenUrl(String),
    /// `electron` alone: the default app's welcome page.
    DefaultApp,
}

/// The usage `-h` prints, after Electron's default app.
pub const USAGE: &str = "\
Electron v39.2.0 - Build cross platform desktop apps with JavaScript, HTML, and CSS
Usage: electron [options] [path]

A path to an Electron app may be specified. It must be one of the following:
  - index.js file.
  - Folder containing a package.json file.
  - Folder containing an index.js file.
  - .html/.htm file.
  - http://, https://, or file:// URL.

Options:
  -i, --interactive     Open a REPL to the main process.
  -r, --require         Module to preload (option can be repeated).
  -v, --version         Print the version.
  -a, --abi             Print the Node ABI version.";

/// What the command line `args` — without the executable's own name — asks for, from the
/// executable `exe`.
///
/// # Errors
///
/// The message Electron shows when the path given is not an app.
pub fn parse(args: Vec<OsString>, exe: &Path, env: &Env) -> Result<Launch, String> {
    if env.is_on("ELECTRON_RUN_AS_NODE") {
        return Ok(Launch::RunAsNode { args });
    }
    let args: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let (resources_path, packaged) = packaged_in(exe);
    let mut start = Start {
        target: Target::DefaultApp,
        switches: switches_of(&args),
        argv: args,
        requires: Vec::new(),
        resources_path,
        manifest: Manifest::default(),
    };
    if let Some(app_path) = packaged {
        start.target = Target::Packaged { app_path };
        return Ok(Launch::Start(Box::new(start)));
    }
    let mut path = None;
    let mut rest = start.argv.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-v" | "--version" => return Ok(Launch::Version),
            "-a" | "--abi" => return Ok(Launch::Abi),
            "-h" | "--help" => return Ok(Launch::Help),
            "-i" | "--interactive" => return Ok(Launch::Interactive),
            "-r" | "--require" => start.requires.extend(rest.next().cloned()),
            // `--` and a lone `-` are no path and no flag the default app knows; the app is the
            // first argument after them that is neither (observed: `electron -- app.js` runs it).
            flag if flag.starts_with('-') => {}
            positional => path = path.or(Some(positional.to_owned())),
        }
        if path.is_some() {
            break;
        }
    }
    if let Some(path) = path {
        (start.target, start.manifest) = target_of(&path)?;
    }
    Ok(Launch::Start(Box::new(start)))
}

/// Where a packaged app's resources are, and its `app.asar` or `app` if it has one.
fn packaged_in(exe: &Path) -> (PathBuf, Option<PathBuf>) {
    let dir = exe.parent().unwrap_or(Path::new("."));
    let resources = if cfg!(target_os = "macos") {
        dir.join("../Resources")
    } else {
        dir.join("resources")
    };
    let app = ["app.asar", "app"]
        .iter()
        .map(|name| resources.join(name))
        .find(|path| path.exists());
    (resources, app)
}

/// The switches of a command line: what starts with `-`, up to a `--`, and is not `-r`'s module.
fn switches_of(args: &[String]) -> Vec<Switch> {
    let mut switches = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            break;
        }
        if arg == "-r" || arg == "--require" {
            rest.next();
        } else if let Some(switch) = arg.strip_prefix("--").or(arg.strip_prefix('-')) {
            let (name, value) = switch.split_once('=').unwrap_or((switch, ""));
            if !name.is_empty() {
                switches.push((name.to_owned(), value.to_owned()));
            }
        }
    }
    switches
}

/// The page the default app shows when it is given nothing to open: the versions this build
/// reports, as a `data:` URL. It is page content, not script.
pub fn welcome_page_url() -> String {
    let html = format!(
        "<!doctype html><html><head><meta charset=utf-8><title>Electron</title></head>         <body style=\"font-family:sans-serif;margin:3em\"><h1>Verve</h1>         <p>Electron {} &middot; Chromium {} &middot; Node.js {}</p>         <p>Run <code>electron path/to/app</code> to start an app.</p></body></html>",
        verve::ELECTRON_VERSION,
        verve::CHROME_VERSION,
        verve::NODE_VERSION
    );
    format!("data:text/html;charset=utf-8,{html}")
}

/// What the first argument that is not a switch names.
fn target_of(given: &str) -> Result<(Target, Manifest), String> {
    let lower = given.to_ascii_lowercase();
    if ["http:", "https:", "file:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        || lower.ends_with(".html")
        || lower.ends_with(".htm")
    {
        return Ok((Target::OpenUrl(given.to_owned()), Manifest::default()));
    }
    let path = std::path::absolute(given).map_err(|error| format!("{given}: {error}"))?;
    if path.is_dir() {
        // A folder is an app by its `package.json`, or by an `index.js` Node would load.
        let manifest = Manifest::read(&path).ok();
        let entry = resolve_main(
            &path,
            manifest
                .as_ref()
                .and_then(|m| m.main.as_deref())
                .unwrap_or("index.js"),
        );
        if manifest.is_none() && !entry.is_file() {
            return Err(not_an_app(&path, "no package.json and no index.js"));
        }
        let target = Target::App {
            app_path: path,
            entry,
        };
        return Ok((target, manifest.unwrap_or_default()));
    }
    if path.is_file() {
        let app_path = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let target = Target::App {
            app_path,
            entry: path,
        };
        return Ok((target, Manifest::default()));
    }
    Err(not_an_app(&path, "no such file or directory"))
}

fn not_an_app(path: &Path, reason: &str) -> String {
    format!(
        "Unable to find Electron app at {}\n\n{reason}",
        path.display()
    )
}

/// The file a package's `main` names, by Node's rules: the file itself, then with `.js`, then
/// the folder's `index.js`. A `main` that names nothing is answered as joined, so loading it
/// reports the missing module.
pub fn resolve_main(dir: &Path, main: &str) -> PathBuf {
    resolve_with(dir, main, |path| path.is_file())
}

fn resolve_with(dir: &Path, main: &str, is_file: impl Fn(&Path) -> bool) -> PathBuf {
    let joined = dir.join(main);
    let found = [
        joined.clone(),
        with_suffix(&joined, ".js"),
        joined.join("index.js"),
    ]
    .into_iter()
    .find(|path| is_file(path));
    found.unwrap_or(joined)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The fields of an app's `package.json` the app object answers from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    /// `name`.
    pub name: Option<String>,
    /// `productName`, which wins over `name`.
    pub product_name: Option<String>,
    /// `version`.
    pub version: Option<String>,
    /// `main`.
    pub main: Option<String>,
}

impl Manifest {
    /// The `package.json` in `dir`, from the disk.
    ///
    /// # Errors
    ///
    /// Why it could not be read or parsed.
    pub fn read(dir: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(dir.join("package.json")).map_err(|e| e.to_string())?;
        Self::parse(&text)
    }

    /// The fields of `package.json`'s text.
    ///
    /// # Errors
    ///
    /// The JSON's own complaint.
    pub fn parse(text: &str) -> Result<Self, String> {
        let json: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let field = |key: &str| json.get(key).and_then(Value::as_str).map(str::to_owned);
        Ok(Self {
            name: field("name"),
            product_name: field("productName"),
            version: field("version"),
            main: field("main"),
        })
    }
}

/// This executable run as Node (`ELECTRON_RUN_AS_NODE`): `args` are Node's, `argv0` the name
/// the executable was started as.
pub fn run_as_node(argv0: OsString, args: Vec<OsString>) -> ExitCode {
    verve::cli::main_node(std::iter::once(argv0).chain(args), |_| Ok(()))
}

/// What an app needs to start: its Hub, told how it was launched and given its `app.asar`, and
/// the program the main process runs first.
///
/// # Errors
///
/// A page of the default app that is not a URL or a file.
pub fn boot(mut start: Start) -> Result<(Arc<Hub>, Program), String> {
    let skip_integrity = start
        .switches
        .iter()
        .any(|(n, _)| n == "verve-skip-asar-integrity");
    let packed = matches!(start.target, Target::Packaged { .. });
    let overlay = Arc::new(AsarOverlay::new(AsarOptions {
        validate_integrity: packed && !skip_integrity,
        on_integrity_failure: Some(Arc::new(|failure| {
            eprintln!(
                "FATAL: Integrity check failed for asar archive: {}",
                failure.message()
            );
            std::process::exit(1);
        })),
        disabled: false,
    }));
    let app_path = match &start.target {
        Target::App { app_path, .. } | Target::Packaged { app_path } => app_path.clone(),
        _ => PathBuf::from("."),
    };
    let hub = Hub::new(ElectronOptions {
        app_path,
        dark: false,
    });
    hub.set_overlay(overlay);
    start_logging(&hub, &start.switches);
    if packed {
        crate::log::diag!(
            hub.log(),
            Asar,
            Info,
            "reading the packed app {}",
            hub.options().app_path.display()
        );
    }
    let program = match &start.target {
        Target::App { entry, .. } => Program::Script(entry.clone()),
        Target::Packaged { app_path } => {
            start.manifest = hub
                .read_text(&app_path.join("package.json"))
                .ok()
                .and_then(|text| Manifest::parse(&text).ok())
                .unwrap_or_default();
            Program::Script(packaged_entry(&hub, app_path, &start.manifest))
        }
        Target::OpenUrl(url) => Program::DefaultApp(Some(page_url(url)?)),
        Target::DefaultApp => Program::DefaultApp(None),
    };
    hub.set_launch(start);
    Ok((hub, program))
}

/// The switches that name a debugger or a Chromium log setting Verve has none of. They are kept in
/// `process.argv` and `app.commandLine` like any switch, and each is told once at the start.
const UNSUPPORTED_SWITCHES: [&str; 10] = [
    "inspect",
    "inspect-brk",
    "inspect-port",
    "inspect-publish-uid",
    "remote-debugging-port",
    "remote-debugging-pipe",
    "js-flags",
    "log-level",
    "v",
    "vmodule",
];

/// The lines to say at the start about switches Verve accepts and cannot honour: one for each
/// name, in the order they were given.
pub fn unsupported_switch_warnings(switches: &[Switch]) -> Vec<String> {
    let mut told: Vec<&str> = Vec::new();
    for (name, _) in switches {
        let name = name.as_str();
        if UNSUPPORTED_SWITCHES.contains(&name) && !told.contains(&name) {
            told.push(name);
        }
    }
    let line = |name: &&str| {
        format!(
            "Verve: --{name} is not supported; see VERVE_ELECTRON_LOG and webContents.openDevTools()"
        )
    };
    told.iter().map(line).collect()
}

/// Turns on what `--enable-logging` and `ELECTRON_ENABLE_LOGGING` ask for (a page's console on
/// the terminal), gets a terminal to write to when anything will go there, and says the
/// unsupported switches. Both write to standard error, where Electron's `=file` forms write to a
/// file instead.
fn start_logging(hub: &Hub, switches: &[Switch]) {
    let log = hub.log();
    if switches.iter().any(|(name, _)| name == "enable-logging") {
        log.set_console(true);
    }
    let warnings = unsupported_switch_warnings(switches);
    if log.writes_anything() || !warnings.is_empty() {
        crate::log::attach_parent_console();
    }
    for line in warnings {
        log.terminal(&line);
    }
}

/// The entry of a packaged app: the `main` of its `package.json`, found in its `app.asar` or
/// `app` folder as in any folder.
fn packaged_entry(hub: &Hub, app_path: &Path, manifest: &Manifest) -> PathBuf {
    let is_file = |path: &Path| match hub.overlay().and_then(|overlay| overlay.stat(path)) {
        Some(Ok(meta)) => meta.kind == verve::vfs::Kind::File,
        _ => path.is_file(),
    };
    resolve_with(
        app_path,
        manifest.main.as_deref().unwrap_or("index.js"),
        is_file,
    )
}

/// The address the default app opens for `electron <url | file.html>`: a URL as it is, a file as
/// the `file:` URL it names.
fn page_url(given: &str) -> Result<String, String> {
    if given.contains("://") || given.starts_with("file:") {
        return Ok(given.to_owned());
    }
    let path = std::path::absolute(given).map_err(|error| format!("{given}: {error}"))?;
    url::Url::from_file_path(&path)
        .map(String::from)
        .map_err(|()| format!("{given} is not a path"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn start(list: &[&str], exe: &Path) -> Start {
        match parse(args(list), exe, &Env::default()).expect("a launch") {
            Launch::Start(start) => *start,
            other => panic!("not an app: {other:?}"),
        }
    }

    /// A scratch folder named for the test.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("verve-launcher-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch folder");
        dir
    }

    fn exe_in(dir: &Path) -> PathBuf {
        dir.join("bin").join("electron.exe")
    }

    /// Electron 39.8.10: `IsEnvSet` reads any value but the empty one as set, so
    /// `ELECTRON_RUN_AS_NODE=0` runs as Node.
    #[test]
    fn run_as_node_takes_every_argument_unless_it_is_empty() {
        let on = Env::default().with("ELECTRON_RUN_AS_NODE", "1");
        let launch = parse(args(&["-e", "1"]), Path::new("e.exe"), &on);
        assert_eq!(
            launch,
            Ok(Launch::RunAsNode {
                args: args(&["-e", "1"])
            })
        );
        let zero = Env::default().with("ELECTRON_RUN_AS_NODE", "0");
        let launch = parse(args(&["-e", "1"]), Path::new("e.exe"), &zero);
        assert!(matches!(launch, Ok(Launch::RunAsNode { .. })));
        let empty = Env::default().with("ELECTRON_RUN_AS_NODE", "");
        let launch = parse(args(&["--version"]), Path::new("e.exe"), &empty);
        assert_eq!(launch, Ok(Launch::Version));
    }

    #[test]
    fn the_flags_of_the_default_app_are_answered() {
        for (flag, want) in [
            ("-v", Launch::Version),
            ("--version", Launch::Version),
            ("-a", Launch::Abi),
            ("--abi", Launch::Abi),
            ("-h", Launch::Help),
            ("--help", Launch::Help),
            ("-i", Launch::Interactive),
            ("--interactive", Launch::Interactive),
        ] {
            let launch = parse(args(&[flag]), Path::new("e.exe"), &Env::default());
            assert_eq!(launch, Ok(want), "{flag}");
        }
    }

    #[test]
    fn no_arguments_is_the_default_app() {
        let start = start(&[], Path::new("nowhere/electron.exe"));
        assert_eq!(start.target, Target::DefaultApp);
        assert!(start.argv.is_empty() && start.switches.is_empty());
    }

    #[test]
    fn a_folder_runs_the_main_of_its_package_json() {
        let dir = scratch("folder");
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"a","main":"lib/start"}"#,
        )
        .unwrap();
        std::fs::create_dir(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/start.js"), "").unwrap();
        let given = dir.to_string_lossy().into_owned();
        let start = start(&[&given, "one"], Path::new("e.exe"));
        assert_eq!(
            start.target,
            Target::App {
                app_path: dir.clone(),
                entry: dir.join("lib/start.js")
            }
        );
        assert_eq!(start.argv, [given, "one".to_owned()]);
    }

    #[test]
    fn a_folder_without_main_runs_its_index() {
        let dir = scratch("index");
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        let given = dir.to_string_lossy().into_owned();
        let Target::App { entry, .. } = start(&[&given], Path::new("e.exe")).target else {
            panic!("an app");
        };
        assert_eq!(entry, dir.join("index.js"));
    }

    /// Electron 39.8.10: a folder with an `index.js` and no `package.json` runs, named "Electron";
    /// a folder with neither is no app.
    #[test]
    fn a_folder_with_only_an_index_runs_and_one_with_nothing_is_no_app() {
        let dir = scratch("only-index");
        std::fs::write(dir.join("index.js"), "").unwrap();
        let given = dir.to_string_lossy().into_owned();
        let started = start(&[&given], Path::new("e.exe"));
        assert_eq!(
            started.target,
            Target::App {
                app_path: dir.clone(),
                entry: dir.join("index.js")
            }
        );
        assert_eq!(started.manifest, Manifest::default());
        let bare = scratch("bare");
        let given = bare.to_string_lossy().into_owned();
        let error = parse(args(&[&given]), Path::new("e.exe"), &Env::default()).unwrap_err();
        assert!(
            error.starts_with("Unable to find Electron app at "),
            "{error}"
        );
    }

    /// Electron 39.8.10: an app given as a script is "Electron" whatever package.json is beside it.
    #[test]
    fn a_script_does_not_take_its_name_from_the_package_json_beside_it() {
        let dir = scratch("beside");
        std::fs::write(dir.join("package.json"), r#"{"name":"beside"}"#).unwrap();
        std::fs::write(dir.join("m.js"), "").unwrap();
        let given = dir.join("m.js").to_string_lossy().into_owned();
        assert_eq!(
            start(&[&given], Path::new("e.exe")).manifest,
            Manifest::default()
        );
        let folder = dir.to_string_lossy().into_owned();
        let named = start(&[&folder], Path::new("e.exe")).manifest;
        assert_eq!(named.name.as_deref(), Some("beside"));
    }

    #[test]
    fn a_script_is_the_entry_and_its_folder_is_the_app() {
        let dir = scratch("script");
        let script = dir.join("main.mjs");
        std::fs::write(&script, "").unwrap();
        let given = script.to_string_lossy().into_owned();
        let start = start(&[&given], Path::new("e.exe"));
        assert_eq!(
            start.target,
            Target::App {
                app_path: dir,
                entry: script
            }
        );
    }

    #[test]
    fn a_missing_path_is_no_app() {
        let error = parse(
            args(&["no-such-app-anywhere"]),
            Path::new("e.exe"),
            &Env::default(),
        );
        assert!(
            error
                .unwrap_err()
                .starts_with("Unable to find Electron app at ")
        );
    }

    #[test]
    fn urls_and_html_files_open_in_the_default_app() {
        for given in [
            "https://example.com/",
            "HTTP://a",
            "file:///x/y.html",
            "page.html",
            "p.HTM",
        ] {
            let start = start(&[given], Path::new("e.exe"));
            assert_eq!(start.target, Target::OpenUrl(given.to_owned()), "{given}");
        }
    }

    #[test]
    fn switches_are_kept_and_not_taken_for_the_app() {
        let dir = scratch("switches");
        let script = dir.join("m.js");
        std::fs::write(&script, "").unwrap();
        let given = script.to_string_lossy().into_owned();
        let start = start(
            &[
                "--inspect=9229",
                "--no-sandbox",
                "-r",
                "./pre.js",
                &given,
                "--lang=de",
                "x",
                "--",
                "--y",
            ],
            Path::new("e.exe"),
        );
        assert!(matches!(start.target, Target::App { .. }));
        assert_eq!(start.requires, ["./pre.js"]);
        let switches: Vec<_> = start
            .switches
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            switches,
            [("inspect", "9229"), ("no-sandbox", ""), ("lang", "de")]
        );
        assert_eq!(start.argv.last().map(String::as_str), Some("--y"));
        assert_eq!(start.argv.len(), 9);
    }

    #[test]
    fn each_unsupported_debugging_switch_is_told_once() {
        let switch = |name: &str, value: &str| (name.to_owned(), value.to_owned());
        let given = [
            switch("inspect", "9229"),
            switch("no-sandbox", ""),
            switch("js-flags", "--max-old-space-size=100"),
            switch("inspect", "9230"),
            switch("enable-logging", ""),
        ];
        assert_eq!(
            unsupported_switch_warnings(&given),
            [
                "Verve: --inspect is not supported; see VERVE_ELECTRON_LOG and webContents.openDevTools()",
                "Verve: --js-flags is not supported; see VERVE_ELECTRON_LOG and webContents.openDevTools()",
            ]
        );
        assert!(unsupported_switch_warnings(&[switch("enable-logging", "")]).is_empty());
    }

    /// Electron 39.8.10: `electron -- --version` prints the version and `electron -- app.js` runs
    /// the app: `--` and a lone `-` are skipped, not obeyed.
    #[test]
    fn a_double_dash_and_a_lone_dash_are_skipped() {
        let version = parse(
            args(&["--", "--version"]),
            Path::new("e.exe"),
            &Env::default(),
        );
        assert_eq!(version, Ok(Launch::Version));
        let dir = scratch("dashes");
        std::fs::write(dir.join("m.js"), "").unwrap();
        let given = dir.join("m.js").to_string_lossy().into_owned();
        let started = start(&["-", "--", &given], Path::new("e.exe"));
        assert!(matches!(started.target, Target::App { .. }));
    }

    /// Electron 39.8.10: `-r` with no module starts the default app.
    #[test]
    fn a_require_flag_without_a_module_is_ignored() {
        let started = start(&["-r"], Path::new("e.exe"));
        assert_eq!(started.target, Target::DefaultApp);
        assert!(started.requires.is_empty());
    }

    #[test]
    fn an_app_beside_the_executable_takes_every_argument() {
        let dir = scratch("packaged");
        let exe = exe_in(&dir);
        std::fs::create_dir_all(dir.join("bin/resources/app")).unwrap();
        let start = start(&["--version", "file.txt", "--lang=fr"], &exe);
        assert_eq!(
            start.target,
            Target::Packaged {
                app_path: dir.join("bin/resources/app")
            }
        );
        assert_eq!(start.argv, ["--version", "file.txt", "--lang=fr"]);
        assert_eq!(
            start.switches,
            [
                ("version".to_owned(), String::new()),
                ("lang".to_owned(), "fr".to_owned())
            ]
        );
        assert_eq!(start.resources_path, dir.join("bin/resources"));
    }

    #[test]
    fn app_asar_is_found_before_an_app_folder() {
        let dir = scratch("asar");
        let exe = exe_in(&dir);
        std::fs::create_dir_all(dir.join("bin/resources/app")).unwrap();
        std::fs::write(dir.join("bin/resources/app.asar"), "").unwrap();
        let Target::Packaged { app_path } = start(&[], &exe).target else {
            panic!("packaged");
        };
        assert_eq!(app_path, dir.join("bin/resources/app.asar"));
    }

    #[test]
    fn a_manifest_reads_the_fields_the_app_answers_from() {
        let manifest = Manifest::parse(
            r#"{"name":"n","productName":"P N","version":"1.2.3","main":"m.js","x":1}"#,
        );
        assert_eq!(
            manifest,
            Ok(Manifest {
                name: Some("n".into()),
                product_name: Some("P N".into()),
                version: Some("1.2.3".into()),
                main: Some("m.js".into()),
            })
        );
        assert!(Manifest::parse("{").is_err());
    }

    #[test]
    fn the_abi_the_launcher_prints_is_the_one_process_reports() {
        assert_eq!(verve::MODULES_VERSION, "140");
        assert!(USAGE.starts_with(&format!("Electron v{}", verve::ELECTRON_VERSION)));
    }
}
