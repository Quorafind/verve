//! The pictures of a page, decoded.
//!
//! The document keeps the bytes of every image the page loaded (`Snapshot::images`). The shell
//! draws decoded pixels, so each address the display list names is handed to one decoding thread
//! (never a thread an image) and the result comes back to the page's thread, which publishes the
//! lot as an [`ImageTable`]. An address the list stops naming, or the document no longer holds,
//! leaves the table. The decoder wakes the page when it has finished a picture.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use verve_dom::canvas::{CanvasFrame, ImageBytes, decode, is_svg, unpremultiply};
use verve_dom::{NodeId, Rect};
use verve_layout::{
    DisplayList, Fill, ImageFrames, ImageKey, ImageTable, PaintItem, canvas_key, key_for_url,
};

use crate::page_frames::{animated, frame_at};

/// Pictures with more pixels than this are not decoded: their pixels alone would take 256 MiB
/// of RAM as BGRA.
const MOST_PIXELS: u64 = 64 << 20;

/// How long the list is still before the pictures far from the window are let go of.
const RELEASE_AFTER: Duration = Duration::from_secs(1);

/// How many bytes of decoded pixels are kept for pictures the display list does not name now
/// (hidden, or swapped by a `:hover`), so that showing them again is not blank. The least
/// recently named go first.
const IDLE_BUDGET: usize = 128 << 20;

struct Job {
    key: ImageKey,
    /// Which bytes the job is for: the address of the allocation the document holds.
    version: usize,
    mime: Option<String>,
    bytes: Arc<[u8]>,
    /// The size the header gave, checked before any pixel is decoded.
    natural: (u32, u32),
}

struct Done {
    key: ImageKey,
    version: usize,
    /// `None` for bytes that do not decode.
    frames: Option<ImageFrames>,
}

/// The key of an address, found again by the allocation of the bytes it was made for, so that
/// the address (a `data:` one can be megabytes) is hashed once and not once a frame.
struct Known {
    key: ImageKey,
    /// Kept so the allocation cannot be freed and its address reused while it is a name.
    _bytes: Arc<[u8]>,
}

/// Decodes a page's pictures off the page's thread and keeps the table of what came back.
pub(crate) struct PageImages {
    jobs: Option<Sender<Job>>,
    done: Option<Receiver<Done>>,
    waker: Option<verve::Waker>,
    known: HashMap<usize, Known>,
    /// The keys the display list names.
    named: HashSet<ImageKey>,
    /// The bytes each key was handed to the decoder for, decoded or not.
    sent: HashMap<ImageKey, usize>,
    inflight: HashSet<ImageKey>,
    decoded: HashMap<ImageKey, Arc<ImageFrames>>,
    /// The SVG pictures the document holds, as text for the shell to draw at its own scale.
    vectors: HashMap<ImageKey, Arc<[u8]>>,
    /// The surfaces of the canvases the list names, at the generation it names.
    surfaces: HashMap<ImageKey, Arc<ImageFrames>>,
    /// When each animated picture the list names first appeared.
    clocks: HashMap<ImageKey, Instant>,
    /// Since when the window has not been drawn: animations stand still, and pick up where
    /// they were when it is.
    hidden: Option<Instant>,
    /// The list the frames were written into, the list it was made from and the frames.
    patched: Option<Patched>,
    /// Every picture the current list draws, near the window or not.
    listed: HashSet<ImageKey>,
    /// When the list last changed: the pictures it draws far from the window are let go of once it
    /// has been still for [`RELEASE_AFTER`].
    list_changed: Instant,
    /// When (by `clock`) each decoded key stopped being named.
    idle: HashMap<ImageKey, u64>,
    clock: u64,
    published: Arc<ImageTable>,
    changed: bool,
}

impl PageImages {
    pub(crate) fn new() -> Self {
        Self {
            jobs: None,
            done: None,
            waker: None,
            known: HashMap::new(),
            named: HashSet::new(),
            sent: HashMap::new(),
            inflight: HashSet::new(),
            decoded: HashMap::new(),
            vectors: HashMap::new(),
            surfaces: HashMap::new(),
            clocks: HashMap::new(),
            patched: None,
            hidden: None,
            listed: HashSet::new(),
            list_changed: Instant::now(),
            idle: HashMap::new(),
            clock: 0,
            published: Arc::new(ImageTable::default()),
            changed: false,
        }
    }

    /// Has the decoder wake the page's isolate when it has finished a picture.
    pub(crate) fn set_waker(&mut self, waker: verve::Waker) {
        self.waker = Some(waker);
        // A thread started before the waker was known cannot use it: the next job starts another.
        self.lose_thread();
    }

    /// Whether the decoder wakes the page itself, so that it need not be asked to come back.
    pub(crate) fn wakes(&self) -> bool {
        self.waker.is_some()
    }

    /// Takes in what the decoder finished, hands it the pictures of `held` that `list` names and
    /// it has not seen (or has seen other bytes of), and drops the ones nothing names or holds.
    /// `list` is given when it is not the one last given.
    pub(crate) fn sync(
        &mut self,
        held: &HashMap<String, ImageBytes>,
        canvases: &HashMap<NodeId, CanvasFrame>,
        (list, viewport): (Option<&DisplayList>, (f32, f32)),
    ) {
        if let Some(list) = list {
            self.named = names_in(list, viewport);
            self.listed = names_in(list, (0., 0.));
            self.list_changed = Instant::now();
        }
        self.sync_surfaces(canvases);
        self.receive();
        let mut live = HashSet::with_capacity(held.len());
        let mut keys = HashSet::with_capacity(held.len());
        for (url, image) in held {
            let version = Arc::as_ptr(&image.bytes).cast::<u8>() as usize;
            live.insert(version);
            let known = self.known.entry(version).or_insert_with(|| Known {
                key: key_for_url(url),
                _bytes: Arc::clone(&image.bytes),
            });
            let key = known.key;
            keys.insert(key);
            if self.named.contains(&key)
                && self.sent.get(&key) != Some(&version)
                && is_svg(&image.bytes)
            {
                // A drawing is rasterised by the shell, at the size it draws it.
                self.vectors.insert(key, Arc::clone(&image.bytes));
                self.sent.insert(key, version);
                self.changed = true;
            } else if self.named.contains(&key) && self.sent.get(&key) != Some(&version) {
                let job = Job {
                    key,
                    version,
                    mime: image.mime.clone(),
                    bytes: Arc::clone(&image.bytes),
                    natural: image.natural,
                };
                if self.send(job) {
                    self.sent.insert(key, version);
                }
            }
        }
        self.known.retain(|version, _| live.contains(version));
        // What the document no longer holds goes; what it holds but nothing names stays, within
        // a budget, for the next time a list names it.
        self.sent.retain(|key, _| keys.contains(key));
        let before = self.decoded.len() + self.vectors.len();
        self.decoded.retain(|key, _| keys.contains(key));
        self.vectors.retain(|key, _| keys.contains(key));
        self.clock += 1;
        for key in self.decoded.keys() {
            if self.named.contains(key) {
                self.idle.remove(key);
            } else {
                self.idle.entry(*key).or_insert(self.clock);
            }
        }
        self.idle.retain(|key, _| self.decoded.contains_key(key));
        self.release_far();
        self.trim_idle();
        self.changed |= self.decoded.len() + self.vectors.len() != before;
    }

    /// Gives the table the pixels of the canvases the list names, at the generation it names, and
    /// lets go of every other generation.
    fn sync_surfaces(&mut self, canvases: &HashMap<NodeId, CanvasFrame>) {
        let mut live = HashSet::new();
        for (node, frame) in canvases {
            let key = canvas_key(*node, frame.generation);
            if !self.named.contains(&key) {
                continue;
            }
            live.insert(key);
            if let std::collections::hash_map::Entry::Vacant(slot) = self.surfaces.entry(key) {
                slot.insert(Arc::new(surface(frame)));
                self.changed = true;
            }
        }
        let before = self.surfaces.len();
        self.surfaces.retain(|key, _| live.contains(key));
        self.changed |= self.surfaces.len() != before;
    }

    /// Lets go of the pixels of the pictures the list draws but not near the window, once the list
    /// has been still for [`RELEASE_AFTER`]: a page that was scrolled away from its pictures gives
    /// their memory back. (A picture the list does not draw at all, hidden or swapped by a
    /// `:hover`, is kept in the idle budget instead.)
    fn release_far(&mut self) {
        if self.list_changed.elapsed() < RELEASE_AFTER {
            return;
        }
        let far: Vec<ImageKey> = self
            .decoded
            .keys()
            .filter(|key| self.listed.contains(key) && !self.named.contains(key))
            .copied()
            .collect();
        for key in far {
            self.decoded.remove(&key);
            self.sent.remove(&key);
            self.idle.remove(&key);
            self.changed = true;
        }
    }

    /// How long until the far pictures are let go of, if there are any to be.
    pub(crate) fn next_release(&self) -> Option<Duration> {
        let any = self
            .decoded
            .keys()
            .any(|key| self.listed.contains(key) && !self.named.contains(key));
        any.then(|| {
            RELEASE_AFTER
                .saturating_sub(self.list_changed.elapsed())
                .max(Duration::from_millis(1))
        })
    }

    /// Drops the pixels of the longest unnamed pictures until the unnamed ones fit the budget.
    fn trim_idle(&mut self) {
        let size = |frames: &ImageFrames| -> usize {
            frames.frames.iter().map(|(bytes, _)| bytes.len()).sum()
        };
        let mut held: usize = self
            .idle
            .keys()
            .filter_map(|key| self.decoded.get(key))
            .map(|frames| size(frames))
            .sum();
        while held > IDLE_BUDGET {
            let Some((&oldest, _)) = self.idle.iter().min_by_key(|(_, since)| **since) else {
                return;
            };
            self.idle.remove(&oldest);
            self.sent.remove(&oldest);
            if let Some(frames) = self.decoded.remove(&oldest) {
                held = held.saturating_sub(size(&frames));
            }
        }
    }

    /// Hands `job` to the decoder; answers whether it went.
    fn send(&mut self, job: Job) -> bool {
        if self.jobs.is_none() && !self.start() {
            return false;
        }
        let key = job.key;
        let sent = self
            .jobs
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok());
        if sent {
            self.inflight.insert(key);
        }
        sent
    }

    /// Forgets the decoding thread and the pictures it had yet to answer, which are asked again.
    fn lose_thread(&mut self) {
        for key in self.inflight.drain() {
            self.sent.remove(&key);
        }
        self.jobs = None;
        self.done = None;
    }

    /// Starts the decoding thread; answers whether there is one.
    fn start(&mut self) -> bool {
        let (jobs, queue) = channel::<Job>();
        let (finished, done) = channel::<Done>();
        let waker = self.waker.clone();
        let started = std::thread::Builder::new()
            .name("verve-image-decoder".into())
            .spawn(move || {
                for job in queue {
                    // A decoder that panics on a picture is that picture's loss, not the page's.
                    let frames = catch_unwind(AssertUnwindSafe(|| still(&job))).unwrap_or(None);
                    let answer = Done {
                        key: job.key,
                        version: job.version,
                        frames,
                    };
                    if finished.send(answer).is_err() {
                        return;
                    }
                    if let Some(wake) = &waker {
                        wake();
                    }
                }
            });
        if started.is_ok() {
            self.jobs = Some(jobs);
            self.done = Some(done);
        }
        started.is_ok()
    }

    fn receive(&mut self) {
        loop {
            let Some(done) = &self.done else { return };
            match done.try_recv() {
                Ok(finished) => {
                    self.inflight.remove(&finished.key);
                    self.take(finished);
                }
                Err(TryRecvError::Empty) => return,
                // The thread is gone with jobs unanswered: they are asked again, by a new
                // thread, so the page is not waited on for ever.
                Err(TryRecvError::Disconnected) => {
                    self.lose_thread();
                    return;
                }
            }
        }
    }

    fn take(&mut self, finished: Done) {
        // A picture dropped, or sent again with other bytes, while it was decoding is not wanted.
        if self.sent.get(&finished.key) != Some(&finished.version) {
            return;
        }
        if let Some(frames) = finished.frames {
            self.decoded.insert(finished.key, Arc::new(frames));
            self.changed = true;
        }
    }

    /// How many pictures the decoder has yet to answer.
    pub(crate) fn pending(&self) -> usize {
        self.inflight.len()
    }

    /// The table of what is decoded, and whether it is not the one last given.
    pub(crate) fn table(&mut self) -> (Arc<ImageTable>, bool) {
        let changed = std::mem::take(&mut self.changed);
        if changed {
            let mut pictures = self.decoded.clone();
            pictures.extend(self.surfaces.iter().map(|(key, s)| (*key, Arc::clone(s))));
            self.published = Arc::new(ImageTable::new(pictures).with_vectors(self.vectors.clone()));
        }
        (Arc::clone(&self.published), changed)
    }
}

/// The frames of the animated pictures the list names, written into it, and whether that is not
/// the list handed out last. `base` is the list the layout made.
struct Patched {
    base: u64,
    frames: HashMap<ImageKey, u32>,
    list: Arc<DisplayList>,
}

impl PageImages {
    /// The list to show at `now`: `base` with the frame of every animated picture in it set to
    /// the one that is on show, and whether it is another list than the last one handed out. A
    /// picture that is not named is not timed, and starts again from its first frame when it is.
    pub(crate) fn animate(
        &mut self,
        base: &Arc<DisplayList>,
        now: Instant,
    ) -> (Arc<DisplayList>, bool) {
        let now = self.hidden.unwrap_or(now);
        let mut frames = HashMap::new();
        let table = &self.decoded;
        let named = &self.named;
        // A clock outlives the picture's being near the window: a finite animation scrolled away
        // and back is where it was, not at its first frame again.
        let listed = &self.listed;
        self.clocks.retain(|key, _| listed.contains(key));
        for key in named {
            let Some(picture) = table.get(key).filter(|p| p.frames.len() > 1) else {
                continue;
            };
            let start = *self.clocks.entry(*key).or_insert(now);
            frames.insert(*key, frame_at(picture, now - start).0);
        }
        if frames.is_empty() {
            let was = self.patched.take().is_some();
            return (Arc::clone(base), was);
        }
        if let Some(last) = &self.patched
            && last.base == base.revision
            && last.frames == frames
        {
            return (Arc::clone(&last.list), false);
        }
        let list = Arc::new(base.with_frames(&frames));
        self.patched = Some(Patched {
            base: base.revision,
            frames,
            list: Arc::clone(&list),
        });
        (list, true)
    }

    /// The window is not drawn (`hidden`) or is again: the animations do not move while it is
    /// not, and go on from the frame they stopped on.
    pub(crate) fn set_hidden(&mut self, hidden: bool, now: Instant) {
        match (hidden, self.hidden) {
            (true, None) => self.hidden = Some(now),
            (false, Some(since)) => {
                for start in self.clocks.values_mut() {
                    *start += now - since;
                }
                self.hidden = None;
            }
            _ => {}
        }
    }

    /// How long until an animated picture on show changes its frame; nothing while the window is
    /// not drawn.
    pub(crate) fn next_frame(&self, now: Instant) -> Option<Duration> {
        if self.hidden.is_some() {
            return None;
        }
        self.clocks
            .iter()
            .filter(|(key, _)| self.named.contains(key))
            .filter_map(|(key, start)| {
                let picture = self.decoded.get(key)?;
                frame_at(picture, now - *start).1
            })
            .min()
    }
}

/// The pixels of a canvas as the shell draws them: straight-alpha BGRA.
fn surface(frame: &CanvasFrame) -> ImageFrames {
    let mut pixels = frame.pixels.to_vec();
    unpremultiply(&mut pixels);
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    ImageFrames {
        width: frame.width,
        height: frame.height,
        frames: vec![(Arc::from(pixels), Duration::ZERO)],
        loops: 1,
    }
}

/// The keys of the pictures `list` draws within a viewport of `viewport` of the window (a viewport
/// above it and one below, as many to either side): a page of hundreds of pictures decodes the
/// few that are in or about to come into view. A viewport of nothing names them all.
fn names_in(list: &DisplayList, viewport: (f32, f32)) -> HashSet<ImageKey> {
    let mut named = HashSet::new();
    let near = (viewport.0 > 0. && viewport.1 > 0.)
        .then(|| Rect::new(-viewport.0, -viewport.1, viewport.0 * 3., viewport.1 * 3.));
    for placed in list.placed(&|_| None) {
        let (bounds, keys): (Rect, Vec<ImageKey>) = match placed.item {
            PaintItem::Image { image, bounds, .. } => (*bounds, vec![*image]),
            PaintItem::Box {
                background, bounds, ..
            } => (*bounds, background.iter().filter_map(picture_of).collect()),
            _ => continue,
        };
        let at = Rect::new(
            bounds.x - placed.shift[0],
            bounds.y - placed.shift[1],
            bounds.width,
            bounds.height,
        );
        if near.is_none_or(|near| near.intersection(&at).is_some()) {
            named.extend(keys);
        }
    }
    named
}

fn picture_of(fill: &Fill) -> Option<ImageKey> {
    match fill {
        Fill::Picture { image, .. } => Some(*image),
        _ => None,
    }
}

/// The picture of `job` as the shell draws it: straight-alpha BGRA.
fn still(job: &Job) -> Option<ImageFrames> {
    if u64::from(job.natural.0) * u64::from(job.natural.1) > MOST_PIXELS {
        return None;
    }
    if let Some(frames) = animated(&job.bytes) {
        return Some(frames);
    }
    let image = decode(&job.bytes, job.mime.as_deref()).ok()?;
    let mut pixels = image.pixels.to_vec();
    unpremultiply(&mut pixels);
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(ImageFrames {
        width: image.width,
        height: image.height,
        frames: vec![(Arc::from(pixels), Duration::ZERO)],
        loops: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1x1 PNG of one opaque red pixel.
    const RED: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    fn held(url: &str, bytes: &[u8]) -> HashMap<String, ImageBytes> {
        let image = ImageBytes {
            mime: Some("image/png".into()),
            bytes: Arc::from(bytes),
            natural: (1, 1),
        };
        HashMap::from([(url.to_owned(), image)])
    }

    fn list_naming(urls: &[&str]) -> DisplayList {
        DisplayList {
            revision: 1,
            items: urls
                .iter()
                .map(|url| PaintItem::Image {
                    node: 1,
                    bounds: verve_dom::Rect::new(0., 0., 1., 1.),
                    clip: verve_dom::Rect::new(0., 0., 1., 1.),
                    image: key_for_url(url),
                    fit: verve_layout::ObjectFit::Fill,
                    frame: 0,
                })
                .collect(),
            rasters: Vec::new(),
            vectors: Vec::new(),
            root: None,
        }
    }

    fn settle(images: &mut PageImages, page: &HashMap<String, ImageBytes>, list: &DisplayList) {
        images.sync(page, &HashMap::new(), (Some(list), (0., 0.)));
        for _ in 0..500 {
            if images.pending() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            images.sync(page, &HashMap::new(), (None, (0., 0.)));
        }
    }

    #[test]
    fn a_picture_is_decoded_to_bgra_on_the_decoder_and_leaves_with_its_address() {
        let mut images = PageImages::new();
        let page = held("http://h/red.png", RED);
        settle(&mut images, &page, &list_naming(&["http://h/red.png"]));
        let (table, changed) = images.table();
        assert!(changed);
        let frames = table.get(key_for_url("http://h/red.png")).expect("decoded");
        assert_eq!((frames.width, frames.height), (1, 1));
        // Red and opaque, as BGRA.
        assert_eq!(&*frames.frames[0].0, &[0, 0, 255, 255]);
        assert!(!images.table().1);
        images.sync(&HashMap::new(), &HashMap::new(), (None, (0., 0.)));
        let (table, changed) = images.table();
        assert!(changed && table.is_empty());
    }

    #[test]
    fn only_the_pictures_the_list_names_are_decoded_and_kept() {
        let mut images = PageImages::new();
        let mut page = held("http://h/red.png", RED);
        page.extend(held("http://h/unused.png", RED));
        settle(&mut images, &page, &list_naming(&["http://h/red.png"]));
        let (table, _) = images.table();
        assert_eq!(table.len(), 1);
        assert!(table.get(key_for_url("http://h/unused.png")).is_none());
        // A list that names nothing keeps the pixels the document still holds bytes for (the
        // picture may be shown again); naming it again needs no new decoding.
        images.sync(&page, &HashMap::new(), (Some(&list_naming(&[])), (0., 0.)));
        assert_eq!(images.table().0.len(), 1);
        images.sync(
            &page,
            &HashMap::new(),
            (Some(&list_naming(&["http://h/red.png"])), (0., 0.)),
        );
        assert_eq!(images.pending(), 0);
        assert_eq!(images.table().0.len(), 1);
        // What the document lets go of leaves the table.
        images.sync(&HashMap::new(), &HashMap::new(), (None, (0., 0.)));
        assert!(images.table().0.is_empty());
    }

    #[test]
    fn a_picture_hidden_and_shown_again_is_not_blank() {
        let mut images = PageImages::new();
        let page = held("http://h/a.png", RED);
        settle(&mut images, &page, &list_naming(&["http://h/a.png"]));
        images.table();
        // Not named for a while: the pixels stay, as long as the document holds the bytes.
        images.sync(&page, &HashMap::new(), (Some(&list_naming(&[])), (0., 0.)));
        let (table, changed) = images.table();
        assert!(!changed && table.len() == 1);
        // Named again: it is there at once, with nothing decoded again.
        images.sync(
            &page,
            &HashMap::new(),
            (Some(&list_naming(&["http://h/a.png"])), (0., 0.)),
        );
        assert_eq!(images.pending(), 0);
        assert_eq!(images.table().0.len(), 1);
        // The document letting go of the bytes is what frees them.
        images.sync(&HashMap::new(), &HashMap::new(), (None, (0., 0.)));
        assert!(images.table().0.is_empty());
    }

    #[test]
    fn a_dead_decoder_does_not_keep_the_page_waiting() {
        let mut images = PageImages::new();
        let page = held("http://h/a.png", RED);
        let key = key_for_url("http://h/a.png");
        // A thread that died with the picture in flight.
        let (dead, done) = channel::<Done>();
        drop(dead);
        images.done = Some(done);
        images.inflight.insert(key);
        images.sent.insert(key, 1);
        images.sync(
            &page,
            &HashMap::new(),
            (Some(&list_naming(&["http://h/a.png"])), (0., 0.)),
        );
        // The picture is asked again of a new thread, and decoded.
        settle(&mut images, &page, &list_naming(&["http://h/a.png"]));
        assert_eq!(images.pending(), 0);
        assert_eq!(images.table().0.len(), 1);
    }

    #[test]
    fn new_bytes_under_the_same_address_are_decoded_again() {
        let mut images = PageImages::new();
        let list = list_naming(&["http://h/a.png"]);
        settle(&mut images, &held("http://h/a.png", RED), &list);
        let first = images.table().0;
        let again = held("http://h/a.png", RED);
        settle(&mut images, &again, &list);
        let (second, changed) = images.table();
        assert!(changed && !Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn a_picture_over_the_pixel_budget_is_not_decoded() {
        let mut images = PageImages::new();
        let mut page = held("http://h/huge.png", RED);
        page.get_mut("http://h/huge.png").unwrap().natural = (1 << 14, 1 << 14);
        settle(&mut images, &page, &list_naming(&["http://h/huge.png"]));
        assert!(images.table().0.is_empty());
        assert_eq!(images.pending(), 0);
    }

    #[test]
    fn bytes_that_do_not_decode_make_no_picture() {
        let mut images = PageImages::new();
        let page = held("http://h/bad.png", b"not a picture");
        settle(&mut images, &page, &list_naming(&["http://h/bad.png"]));
        assert!(images.table().0.is_empty());
    }

    #[test]
    fn only_the_pictures_within_a_viewport_of_the_window_are_named() {
        let mut list = list_naming(&["http://h/near.png", "http://h/far.png"]);
        if let PaintItem::Image { bounds, .. } = &mut list.items[1] {
            *bounds = verve_dom::Rect::new(0., 10_000., 1., 1.);
        }
        let named = names_in(&list, (100., 100.));
        assert!(named.contains(&key_for_url("http://h/near.png")));
        assert!(!named.contains(&key_for_url("http://h/far.png")));
        assert_eq!(names_in(&list, (0., 0.)).len(), 2);
    }

    #[test]
    fn an_animation_stands_still_while_the_window_is_not_drawn_and_goes_on_after() {
        let mut images = PageImages::new();
        let key = ImageKey(1);
        let frame = |ms| (Arc::<[u8]>::from(vec![0u8; 4]), Duration::from_millis(ms));
        images.decoded.insert(
            key,
            Arc::new(ImageFrames {
                width: 1,
                height: 1,
                frames: vec![frame(100), frame(100)],
                loops: 0,
            }),
        );
        let start = Instant::now();
        images.clocks.insert(key, start);
        images.named.insert(key);
        assert!(images.next_frame(start).is_some());
        images.set_hidden(true, start + Duration::from_millis(50));
        assert!(images.next_frame(start + Duration::from_secs(9)).is_none());
        // Shown again nine seconds on: it is 50 ms into its first frame still.
        let later = start + Duration::from_secs(9);
        images.set_hidden(false, later);
        assert_eq!(images.next_frame(later), Some(Duration::from_millis(50)));
    }

    #[test]
    fn pictures_far_from_the_window_are_let_go_of_once_the_list_is_still() {
        let mut images = PageImages::new();
        let page = held("http://h/far.png", RED);
        let mut list = list_naming(&["http://h/far.png"]);
        // Near the window while it is named, and decoded.
        settle(&mut images, &page, &list);
        assert_eq!(images.table().0.len(), 1);
        // Scrolled away: in the list, not named. Kept while the list is changing, then let go.
        if let PaintItem::Image { bounds, .. } = &mut list.items[0] {
            *bounds = verve_dom::Rect::new(0., 10_000., 1., 1.);
        }
        images.sync(&page, &HashMap::new(), (Some(&list), (100., 100.)));
        assert_eq!(images.table().0.len(), 1);
        assert!(images.next_release().is_some());
        images.list_changed = Instant::now() - RELEASE_AFTER;
        images.sync(&page, &HashMap::new(), (None, (100., 100.)));
        let (table, changed) = images.table();
        assert!(changed && table.is_empty());
        assert!(images.next_release().is_none());
    }
}
