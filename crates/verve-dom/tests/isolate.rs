//! The document on a thread of its own, as a drawer runs it: a `verve::isolate` guest that owns
//! the runtime and the [`Document`], publishing a [`Snapshot`] with what changed after every
//! turn, and taking what a person does as jobs. The drawer's thread only reads snapshots.

use std::time::Duration;

use verve::Runtime;
use verve::isolate::{Guest, Isolate, Options as IsolateOptions, Outbox};
use verve_dom::{Changes, Document, Options, Request, Snapshot};

struct Page {
    runtime: Runtime,
    document: Document,
}

impl Guest for Page {
    type View = (Snapshot, Changes);
    type Event = Request;

    fn tick(&mut self) {
        self.runtime.run_due();
    }

    fn next_due(&self) -> Option<Duration> {
        let due = self.runtime.next_due()?;
        Some(Duration::from_secs_f64(
            ((due - self.runtime.now()) / 1000.).max(0.),
        ))
    }

    fn drain(&mut self, out: &mut Outbox<Request>) {
        for request in self.document.take_requests() {
            out.push(request);
        }
    }

    fn publish(&mut self, _applied: u64, ran: bool) -> Option<(Snapshot, Changes)> {
        let update = self.document.update();
        (ran || !update.changes.is_empty()).then_some((update.snapshot, update.changes))
    }
}

#[test]
fn a_drawer_reads_snapshots_from_the_documents_own_thread() {
    let isolate = Isolate::spawn(
        IsolateOptions::default(),
        || {
            let mut runtime = Runtime::new();
            let document = verve_dom::install(&mut runtime, Options::default()).unwrap();
            runtime
                .eval(
                    r#"
                    document.body.innerHTML = '<button id="go">0</button>';
                    document.getElementById('go').addEventListener('click', (e) => {
                        e.target.textContent = String(Number(e.target.textContent) + 1);
                        document.title = 'pressed';
                    });
                    "#,
                )
                .unwrap();
            Page { runtime, document }
        },
        || {},
    )
    .unwrap();
    // The first snapshot, read on this thread.
    let job = isolate.send(|_, _| {});
    let (snapshot, _) = isolate.wait_for(job, Duration::from_secs(10)).unwrap();
    let button = snapshot.dom.query(snapshot.dom.document, "#go").unwrap();
    assert_eq!(snapshot.dom.text_content(button), "0");
    assert_eq!(snapshot.style(button, "display"), Some("inline-block"));
    // A press, sent as a job; its effect in the next snapshot, and its request as an event.
    let job = isolate.send(move |page, _| {
        page.document.press(&mut page.runtime, button);
    });
    let (snapshot, changes) = isolate.wait_for(job, Duration::from_secs(10)).unwrap();
    assert_eq!(snapshot.dom.text_content(button), "1");
    assert!(changes.nodes.contains(&button), "{changes:?}");
    let events = isolate.take_events();
    assert!(
        events.contains(&Request::Title("pressed".into())),
        "{events:?}"
    );
}
