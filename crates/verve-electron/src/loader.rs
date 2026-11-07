//! What a page is loaded from: `file:` (through the app's virtual filesystem, so `app.asar`
//! is read), `data:`, and the schemes the main process answers: custom ones, and `file:`, `http:`
//! or `https:` too once `protocol.handle` claims them. The network's own `http`, `https` and
//! `blob` that nothing claims are Verve's `fetch`, which the page's document uses itself.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use url::Url;
use verve::vfs::Overlay;
use verve_dom::{Destination, LoadError, LoadRequest, LoadResult, Loader};

use crate::api::mime;
use crate::hub::Session;
use crate::msg::{FromRenderer, ProtocolRequest, ProtocolResponse};

type Done = Box<dyn FnOnce(LoadResult)>;

/// A page's loader. Cloned: the document holds one and the page's guest the other, which is how
/// the guest hears what the loader asks of the main process.
#[derive(Clone)]
pub struct ElectronLoader {
    overlay: Option<Arc<dyn Overlay>>,
    /// The session the page belongs to: the schemes its main process answers
    /// (`protocol.handle`) are the session's.
    session: Arc<Session>,
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
    pub fn new(
        overlay: Option<Arc<dyn Overlay>>,
        calls: Arc<AtomicU64>,
        session: Arc<Session>,
    ) -> Self {
        Self {
            overlay,
            session,
            outbox: Rc::default(),
            calls: Rc::default(),
            next: calls,
        }
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

    /// Whether the page that asked may read this `file:` address: only a page that is a file
    /// itself may (Electron's `webSecurity`), and only on this machine — a `file://host/share/…`
    /// is refused, since none was seen to load from a file page. The browser's own `loadURL`
    /// (no initiator) may read any. The rule holds whether the file is read here or a `file`
    /// handler answers for it (Electron 39.8.10, probe5: with such a handler an `app://` page
    /// still fails to `fetch`, `XMLHttpRequest`, `<img>` and `<script>` a `file:` address, and
    /// a `loadFile` page still reads it).
    fn file_allowed(request: &LoadRequest) -> bool {
        let local = matches!(request.url.host_str(), None | Some("" | "localhost"));
        let allowed = request
            .initiator
            .as_ref()
            .is_none_or(|from| from.scheme() == "file");
        local && allowed
    }

    fn read_file(&self, url: Url) -> LoadResult {
        let Ok(path) = url.to_file_path() else {
            // Chromium resolves a path the system has no such file for (no drive letter on
            // Windows) and finds nothing there.
            return LoadResult::failed(url, -6, "ERR_FILE_NOT_FOUND");
        };
        match crate::files::read(self.overlay.as_ref(), &path) {
            Ok(bytes) => LoadResult {
                status: 200,
                mime: Some(mime::of_path(&path).to_owned()),
                url,
                headers: Vec::new(),
                body: Ok(bytes),
            },
            Err(error) => LoadResult::failed(url, error.code, error.short_name()),
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
            "file" if !Self::file_allowed(&request) => {
                done(LoadResult::failed(request.url, -10, "ERR_ACCESS_DENIED"))
            }
            scheme if self.session.handles(scheme) => self.ask(request, done),
            "file" => done(self.read_file(request.url)),
            "data" => done(Self::data(request.url)),
            _ => done(LoadResult::failed(
                request.url,
                -302,
                "ERR_UNKNOWN_URL_SCHEME",
            )),
        }
    }

    fn handles(&self, scheme: &str) -> bool {
        matches!(scheme, "file" | "data") || self.session.handles(scheme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loader of a session that answers the given custom schemes.
    fn loader_for(schemes: &[&str], calls: Arc<AtomicU64>) -> ElectronLoader {
        let session = Arc::new(Session::new(1, ""));
        for scheme in schemes {
            session.set_handled(scheme, true);
        }
        ElectronLoader::new(None, calls, session)
    }

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
        let loader = loader_for(&[], Arc::default());
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
        let loader = loader_for(&[], Arc::default());
        let path = std::env::temp_dir().join(format!("verve-p20-{}.css", std::process::id()));
        std::fs::write(&path, "a{}").unwrap();
        let result = answer_of(&loader, Url::from_file_path(&path).unwrap().as_str());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(result.mime.as_deref(), Some("text/css"));
        assert_eq!(result.body.unwrap(), b"a{}");
    }

    #[test]
    fn a_file_handler_is_asked_for_the_pages_that_may_read_files_and_refuses_the_rest() {
        let loader = loader_for(&["file"], Arc::default());
        let load = |initiator: Option<&str>| {
            let slot = Rc::new(RefCell::new(None));
            let sink = Rc::clone(&slot);
            let request = LoadRequest {
                url: Url::parse("file:///data/x.txt").unwrap(),
                initiator: initiator.map(|from| Url::parse(from).unwrap()),
                destination: verve_dom::Destination::Script,
                method: "GET".into(),
                headers: Vec::new(),
                body: None,
            };
            loader.fetch(
                request,
                Box::new(move |result| *sink.borrow_mut() = Some(result)),
            );
            slot.take()
        };
        let refused = load(Some("app://host/")).expect("refused at once");
        assert_eq!(refused.body.unwrap_err().code, -10);
        assert!(loader.take_outbox().is_empty());
        // The browser's own load, and a page that is a file itself, reach the handler.
        for allowed in [None, Some("file:///data/index.html")] {
            assert!(load(allowed).is_none());
            assert_eq!(loader.take_outbox().len(), 1);
        }
    }

    #[test]
    fn a_custom_scheme_is_asked_of_the_main_process_and_answered_once() {
        let loader = loader_for(&[], Arc::default());
        assert!(!loader.handles("app"));
        let loader = loader_for(&["app"], Arc::default());
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
        let loader = loader_for(&[], Arc::default());
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
            loader_for(&["app"], calls.clone()),
            loader_for(&["app"], calls),
        );
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
