//! What a page is loaded from: `file:` (through the app's virtual filesystem, so `app.asar`
//! is read), `data:`, and the custom schemes the main process answers. The network's own
//! `http`, `https` and `blob` are Verve's `fetch`, which the page's document uses itself.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::io::ErrorKind;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use url::Url;
use verve::vfs::Overlay;
use verve_dom::{Destination, LoadError, LoadRequest, LoadResult, Loader};

use crate::msg::{FromRenderer, ProtocolRequest, ProtocolResponse};

type Done = Box<dyn FnOnce(LoadResult)>;

/// A page's loader. Cloned: the document holds one and the page's guest the other, which is how
/// the guest hears what the loader asks of the main process.
#[derive(Clone)]
pub struct ElectronLoader {
    overlay: Option<Arc<dyn Overlay>>,
    /// The custom schemes the main process answers (`protocol.handle`).
    schemes: Rc<RefCell<BTreeSet<String>>>,
    /// Asks for the main process, until the guest sends them.
    outbox: Rc<RefCell<Vec<FromRenderer>>>,
    /// The asks not yet answered, by their number.
    calls: Rc<RefCell<HashMap<u64, (Url, Done)>>>,
    /// The numbers of the asks, shared by every document of the app: an answer that comes
    /// late, for a document that is gone, finds no ask in the document that replaced it.
    next: Arc<AtomicU64>,
}

impl ElectronLoader {
    /// A loader reading files through `overlay` first, then from the disk; its asks are
    /// numbered from `calls`.
    pub fn new(overlay: Option<Arc<dyn Overlay>>, calls: Arc<AtomicU64>) -> Self {
        Self {
            overlay,
            schemes: Rc::default(),
            outbox: Rc::default(),
            calls: Rc::default(),
            next: calls,
        }
    }

    /// Makes `scheme` one the main process answers (P16 registers them).
    pub fn register_scheme(&self, scheme: &str) {
        self.schemes
            .borrow_mut()
            .insert(scheme.to_ascii_lowercase());
    }

    /// How many asks the main process has not answered yet.
    pub fn waiting(&self) -> usize {
        self.calls.borrow().len()
    }

    /// What this loader asks of the main process since last asked.
    pub fn take_outbox(&self) -> Vec<FromRenderer> {
        std::mem::take(&mut *self.outbox.borrow_mut())
    }

    /// The main process's answer to an ask.
    pub fn answer(&self, call: u64, response: ProtocolResponse) {
        let Some((url, done)) = self.calls.borrow_mut().remove(&call) else {
            return;
        };
        done(LoadResult {
            status: response.status,
            url,
            mime: response.mime,
            headers: response.headers,
            body: response
                .body
                .map_err(|(code, description)| LoadError { code, description }),
        });
    }

    /// A `file:` address for the page that asked: only a page that is a file itself may read
    /// one (Electron's `webSecurity`), and only on this machine — a `file://host/share/…` is
    /// refused, since none was seen to load from a file page. The browser's own `loadURL`
    /// (no initiator) may read any.
    fn file_for(&self, request: LoadRequest) -> LoadResult {
        let local = matches!(request.url.host_str(), None | Some("" | "localhost"));
        let allowed = request
            .initiator
            .as_ref()
            .is_none_or(|from| from.scheme() == "file");
        if local && allowed {
            return self.read_file(request.url);
        }
        LoadResult::failed(request.url, -10, "ERR_ACCESS_DENIED")
    }

    fn read_file(&self, url: Url) -> LoadResult {
        let Ok(path) = url.to_file_path() else {
            // Chromium resolves a path the system has no such file for (no drive letter on
            // Windows) and finds nothing there.
            return LoadResult::failed(url, -6, "ERR_FILE_NOT_FOUND");
        };
        let packed = self
            .overlay
            .as_ref()
            .and_then(|overlay| overlay.read(&path));
        let read = match packed {
            Some(read) => read.map(|bytes| bytes.into_owned()),
            None if path.is_dir() => return LoadResult::failed(url, -2, "ERR_FAILED"),
            None => std::fs::read(&path),
        };
        match read {
            Ok(bytes) => LoadResult {
                status: 200,
                mime: Some(mime_of(&path).to_owned()),
                url,
                headers: Vec::new(),
                body: Ok(bytes),
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                LoadResult::failed(url, -6, "ERR_FILE_NOT_FOUND")
            }
            Err(error) if error.kind() == ErrorKind::PermissionDenied => {
                LoadResult::failed(url, -10, "ERR_ACCESS_DENIED")
            }
            Err(_) => LoadResult::failed(url, -2, "ERR_FAILED"),
        }
    }

    fn data(url: Url) -> LoadResult {
        match verve::encoding::data_url(&url) {
            Ok((mime, bytes)) => LoadResult {
                status: 200,
                mime: Some(mime),
                url,
                headers: Vec::new(),
                body: Ok(bytes),
            },
            Err(_) => LoadResult::failed(url, -300, "ERR_INVALID_URL"),
        }
    }

    fn ask(&self, request: LoadRequest, done: Done) {
        let call = self.next.fetch_add(1, Ordering::Relaxed);
        let destination = destination_name(request.destination).to_owned();
        self.calls
            .borrow_mut()
            .insert(call, (request.url.clone(), done));
        self.outbox
            .borrow_mut()
            .push(FromRenderer::ProtocolRequest {
                call,
                request: ProtocolRequest {
                    url: request.url.to_string(),
                    method: request.method,
                    headers: request.headers,
                    body: request.body,
                    destination,
                },
            });
    }
}

/// What a request is for, by the names of Fetch's `destination`: a module is a `script`, an
/// icon an `image`, and a page's own `fetch()` has none.
fn destination_name(destination: Destination) -> &'static str {
    match destination {
        Destination::Document => "document",
        Destination::Script | Destination::Module => "script",
        Destination::Style => "style",
        Destination::Image | Destination::Icon => "image",
        Destination::Font => "font",
        Destination::Fetch => "",
    }
}

impl Loader for ElectronLoader {
    fn fetch(&self, request: LoadRequest, done: Box<dyn FnOnce(LoadResult)>) {
        match request.url.scheme() {
            "file" => done(self.file_for(request)),
            "data" => done(Self::data(request.url)),
            scheme if self.schemes.borrow().contains(scheme) => self.ask(request, done),
            _ => done(LoadResult::failed(
                request.url,
                -302,
                "ERR_UNKNOWN_URL_SCHEME",
            )),
        }
    }

    fn handles(&self, scheme: &str) -> bool {
        matches!(scheme, "file" | "data") || self.schemes.borrow().contains(scheme)
    }
}

/// What a file of this name is, as a server would say.
fn mime_of(path: &Path) -> &'static str {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match extension.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" | "cjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer_of(loader: &ElectronLoader, url: &str) -> LoadResult {
        let slot = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&slot);
        let request = LoadRequest {
            url: Url::parse(url).unwrap(),
            initiator: None,
            destination: verve_dom::Destination::Script,
            method: "GET".into(),
            headers: Vec::new(),
            body: None,
        };
        loader.fetch(
            request,
            Box::new(move |result| *sink.borrow_mut() = Some(result)),
        );
        slot.take().expect("answered at once")
    }

    #[test]
    fn a_missing_file_is_err_file_not_found_and_a_folder_is_a_failure() {
        let loader = ElectronLoader::new(None, Arc::default());
        let dir = std::env::temp_dir();
        let missing = Url::from_file_path(dir.join("verve-p20-missing.html")).unwrap();
        let result = answer_of(&loader, missing.as_str());
        assert_eq!(result.body.unwrap_err().code, -6);
        let folder = Url::from_directory_path(&dir).unwrap();
        assert_eq!(
            answer_of(&loader, folder.as_str()).body.unwrap_err().code,
            -2
        );
    }

    #[test]
    fn a_file_is_read_with_the_type_its_name_says() {
        let loader = ElectronLoader::new(None, Arc::default());
        let path = std::env::temp_dir().join(format!("verve-p20-{}.css", std::process::id()));
        std::fs::write(&path, "a{}").unwrap();
        let result = answer_of(&loader, Url::from_file_path(&path).unwrap().as_str());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(result.mime.as_deref(), Some("text/css"));
        assert_eq!(result.body.unwrap(), b"a{}");
    }

    #[test]
    fn a_custom_scheme_is_asked_of_the_main_process_and_answered_once() {
        let loader = ElectronLoader::new(None, Arc::default());
        assert!(!loader.handles("app"));
        loader.register_scheme("APP");
        assert!(loader.handles("app"));
        let slot = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&slot);
        let request = LoadRequest {
            url: Url::parse("app://host/index.html").unwrap(),
            initiator: None,
            destination: verve_dom::Destination::Document,
            method: "GET".into(),
            headers: Vec::new(),
            body: None,
        };
        loader.fetch(
            request,
            Box::new(move |result| *sink.borrow_mut() = Some(result)),
        );
        let asked = loader.take_outbox();
        let [FromRenderer::ProtocolRequest { call, request }] = &asked[..] else {
            panic!("one ask: {asked:?}");
        };
        assert_eq!(request.url, "app://host/index.html");
        assert!(slot.borrow().is_none());
        let response = ProtocolResponse {
            status: 200,
            mime: Some("text/html".into()),
            headers: Vec::new(),
            body: Ok(b"<p>hi".to_vec()),
        };
        loader.answer(*call, response);
        assert_eq!(slot.take().unwrap().body.unwrap(), b"<p>hi");
    }

    #[test]
    fn only_a_file_page_reads_a_file_and_only_a_local_one() {
        let loader = ElectronLoader::new(None, Arc::default());
        let path = std::env::temp_dir().join(format!("verve-p20-init-{}.txt", std::process::id()));
        std::fs::write(&path, "x").unwrap();
        let url = Url::from_file_path(&path).unwrap();
        let from = |initiator: Option<&str>, url: &Url| {
            let mut request = LoadRequest {
                url: url.clone(),
                initiator: initiator.map(|i| Url::parse(i).unwrap()),
                destination: verve_dom::Destination::Fetch,
                method: "GET".into(),
                headers: Vec::new(),
                body: None,
            };
            request.url = url.clone();
            let slot = Rc::new(RefCell::new(None));
            let sink = Rc::clone(&slot);
            loader.fetch(request, Box::new(move |r| *sink.borrow_mut() = Some(r)));
            slot.take().unwrap().body.map_err(|e| e.code)
        };
        assert!(from(Some("file:///app/index.html"), &url).is_ok());
        assert!(from(None, &url).is_ok());
        for page in ["data:text/html,x", "http://127.0.0.1:1/", "app://host/"] {
            assert_eq!(from(Some(page), &url), Err(-10), "{page}");
        }
        let share = Url::parse("file://server/share/x.txt").unwrap();
        assert_eq!(from(Some("file:///app/index.html"), &share), Err(-10));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn asks_are_numbered_across_documents_and_a_late_answer_finds_nothing() {
        let calls = Arc::new(AtomicU64::new(0));
        let (old, new) = (
            ElectronLoader::new(None, calls.clone()),
            ElectronLoader::new(None, calls),
        );
        old.register_scheme("app");
        new.register_scheme("app");
        let ask = |loader: &ElectronLoader| {
            let slot = Rc::new(RefCell::new(None));
            let sink = Rc::clone(&slot);
            let request = LoadRequest {
                url: Url::parse("app://host/x").unwrap(),
                initiator: None,
                destination: verve_dom::Destination::Fetch,
                method: "GET".into(),
                headers: Vec::new(),
                body: None,
            };
            loader.fetch(request, Box::new(move |r| *sink.borrow_mut() = Some(r)));
            slot
        };
        let _gone = ask(&old);
        let waiting = ask(&new);
        let call_of = |loader: &ElectronLoader| match loader.take_outbox().remove(0) {
            FromRenderer::ProtocolRequest { call, .. } => call,
            other => panic!("{other:?}"),
        };
        let (first, second) = (call_of(&old), call_of(&new));
        assert_ne!(first, second);
        let response = |text: &str| ProtocolResponse {
            status: 200,
            mime: None,
            headers: Vec::new(),
            body: Ok(text.as_bytes().to_vec()),
        };
        new.answer(first, response("late, for the old document"));
        assert!(waiting.borrow().is_none());
        new.answer(second, response("mine"));
        assert_eq!(waiting.take().unwrap().body.unwrap(), b"mine");
    }
}
