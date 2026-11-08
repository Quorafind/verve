//! The inspector window's contents: the element tree on the left, the styles and box model of the
//! selected node on the right, the page's console and the REPL below.
//!
//! The view owns a [`Model`] and draws it with GPUI's own elements. What a person does to it
//! changes the model; what the page window should show of that (the highlight, the picker) is
//! queued for the shell, which owns the page's window.

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    ClickEvent, Context, FocusHandle, KeyDownEvent, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, div, px, rgb, uniform_list,
};
use verve::Level;
use verve_dom::{NodeId, Update};
use verve_electron::Hub;
use verve_electron::ids::ContentsId;
use verve_electron::msg::{InspectorEvent, ToRenderer};
use verve_layout::DisplayList;

use super::boxmodel::{BoxModel, Edges, Sides};
use super::console::{Line, LineKind};
use super::model::{Highlight, Model};
use super::overlay::PickEvent;
use super::styles::StyleLine;
use super::tree::{Row, RowKind};
use crate::shell::{Inbox, Item};

const BACKGROUND: u32 = 0x001e_1e1e;
const PANEL: u32 = 0x0025_2526;
const LINE: u32 = 0x003c_3c3c;
const TEXT: u32 = 0x00d4_d4d4;
const DIM: u32 = 0x0088_8888;
const TAG: u32 = 0x0056_9cd6;
const TEXT_NODE: u32 = 0x00ce_9178;
const COMMENT: u32 = 0x006a_9955;
const SELECTED: u32 = 0x0009_4771;
const HOVERED: u32 = 0x002a_2d2e;
const WARNING: u32 = 0x00e5_c07b;
const ERROR: u32 = 0x00f4_7067;

/// The height of a row of any list.
const ROW: f32 = 20.;

/// How far a level of the tree is indented.
const INDENT: f32 = 14.;

/// One inspector window's state.
pub(crate) struct InspectorView {
    contents: ContentsId,
    hub: Arc<Hub>,
    inbox: Inbox,
    title: SharedString,
    /// Which document of the page the model is about ([`Hub::page_generation`]).
    generation: u64,
    model: Model,
    tree_focus: FocusHandle,
    input_focus: FocusHandle,
    tree_scroll: UniformListScrollHandle,
    console_scroll: UniformListScrollHandle,
    /// What the page window was last told, to say it only when it changes.
    told_highlight: Option<Highlight>,
    told_picking: bool,
}

impl InspectorView {
    pub fn new(
        contents: ContentsId,
        hub: Arc<Hub>,
        inbox: Inbox,
        title: String,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            contents,
            generation: hub.page_generation(contents),
            hub,
            inbox,
            title: title.into(),
            model: Model::new(),
            tree_focus: cx.focus_handle(),
            input_focus: cx.focus_handle(),
            tree_scroll: UniformListScrollHandle::new(),
            console_scroll: UniformListScrollHandle::new(),
            told_highlight: None,
            told_picking: false,
        }
    }

    /// The window gained or lost the system's focus: the main process hears of it.
    pub fn activation(&mut self, active: bool) {
        let event = if active {
            InspectorEvent::Focused
        } else {
            InspectorEvent::Blurred
        };
        let contents = self.contents;
        self.hub
            .from_shell(verve_electron::msg::FromShell::Inspector { contents, event });
    }

    /// The page published a document and a layout, of the document `generation` (read before the
    /// view was taken from the Hub). Something of a document older than the model's is dropped; of
    /// a newer one, the model starts again first.
    pub fn page(
        &mut self,
        generation: u64,
        update: Option<Update>,
        list: Option<Arc<DisplayList>>,
        cx: &mut Context<Self>,
    ) {
        if generation < self.generation {
            return;
        }
        self.new_document_if_any(generation);
        self.model.page(update, list);
        self.publish();
        cx.notify();
    }

    /// Starts again from nothing if the page is showing a newer document than the model's (it
    /// navigated, or reloaded the same address): the selection, folds and waiting questions were
    /// about nodes that are gone. Answers whether it did.
    fn new_document_if_any(&mut self, generation: u64) -> bool {
        if generation <= self.generation {
            return false;
        }
        self.generation = generation;
        self.model.new_document();
        self.publish();
        true
    }

    /// Takes the console lines the page wrote since the last look; answers whether there were any.
    fn take_console(&mut self) -> bool {
        let lines = self
            .hub
            .console_since(self.contents, self.model.console.seen());
        let any = !lines.is_empty();
        self.model.console.take(lines);
        any
    }

    /// Takes the console lines the page wrote since the last look.
    pub fn pump_console(&mut self, cx: &mut Context<Self>) {
        let reset = self.new_document_if_any(self.hub.page_generation(self.contents));
        let lines = self.take_console();
        if lines {
            self.follow_console();
        }
        if reset || lines {
            cx.notify();
        }
    }

    /// The page answered the REPL's `call`. The lines the code wrote come before its answer.
    pub fn reply(&mut self, call: u64, text: String, is_error: bool, cx: &mut Context<Self>) {
        let reset = self.new_document_if_any(self.hub.page_generation(self.contents));
        let lines = self.take_console();
        let answered = self.model.console.answer(call, text, is_error);
        if answered || lines {
            self.follow_console();
        }
        if reset || lines || answered {
            cx.notify();
        }
    }

    /// What the pointer did over the page while a node was being picked.
    pub fn picked(&mut self, event: PickEvent, cx: &mut Context<Self>) {
        match event {
            PickEvent::Hover(node) => self.model.hover(node),
            PickEvent::Click(node) => {
                self.model.picking = false;
                self.model.hover(None);
                if node.is_some() {
                    self.select(node);
                }
            }
        }
        self.publish();
        cx.notify();
    }

    /// Selects `node` and shows it in the tree.
    fn select(&mut self, node: Option<NodeId>) {
        if let Some(row) = self.model.select(node) {
            self.tree_scroll.scroll_to_item(row, ScrollStrategy::Center);
        }
    }

    fn follow_console(&self) {
        let count = self.model.console.lines().len();
        if count > 0 {
            self.console_scroll
                .scroll_to_item(count - 1, ScrollStrategy::Bottom);
        }
    }

    /// Tells the shell what the page window should show now, if that changed.
    fn publish(&mut self) {
        let highlight = self.model.highlight();
        if highlight != self.told_highlight {
            self.told_highlight = highlight;
            self.inbox
                .push_item(Item::Highlight(self.contents, highlight));
        }
        if self.model.picking != self.told_picking {
            self.told_picking = self.model.picking;
            self.inbox
                .push_item(Item::Picking(self.contents, self.model.picking));
        }
    }

    fn toggle_picking(&mut self, cx: &mut Context<Self>) {
        self.model.picking = !self.model.picking;
        if !self.model.picking {
            self.model.hover(None);
        }
        self.publish();
        cx.notify();
    }

    /// The REPL's line is sent to the page.
    fn send_line(&mut self) {
        let Some(code) = self.model.input.submit() else {
            return;
        };
        let call = self.hub.next_inspector_call();
        self.model.console.ask(call, &code);
        if self.hub.has_renderer(self.contents) {
            let message = ToRenderer::InspectorEval { call, code };
            self.hub.to_renderer(self.contents, message);
        } else {
            let gone = "The page is not running.".to_owned();
            self.model.console.answer(call, gone, true);
        }
        self.follow_console();
    }

    fn input_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let input = &mut self.model.input;
        match key.key.as_str() {
            "enter" => self.send_line(),
            "backspace" => input.backspace(),
            "delete" => input.delete(),
            "left" => input.left(),
            "right" => input.right(),
            "home" => input.home(),
            "end" => input.end(),
            "up" => input.older(),
            "down" => input.newer(),
            _ => {
                let typing = !key.modifiers.control && !key.modifiers.platform;
                if let (true, Some(text)) = (typing, key.key_char.as_deref()) {
                    input.insert(text);
                }
            }
        }
        cx.notify();
    }

    fn tree_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(selected) = self.model.selected() else {
            return;
        };
        let tree = &mut self.model.tree;
        let next = match event.keystroke.key.as_str() {
            "down" => tree.neighbour(selected, 1),
            "up" => tree.neighbour(selected, -1),
            "right" => {
                tree.set_folded(selected, false);
                None
            }
            "left" => {
                tree.set_folded(selected, true);
                None
            }
            _ => None,
        };
        if let Some(node) = next {
            self.select(Some(node));
        }
        self.publish();
        cx.notify();
    }

    fn row(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let node = row.node;
        let colour = match row.kind {
            RowKind::Element => TAG,
            RowKind::Text => TEXT_NODE,
            RowKind::Comment => COMMENT,
            RowKind::Doctype => DIM,
        };
        let selected = self.model.selected() == Some(node);
        let hovered = self.model.hovered() == Some(node);
        let arrow = match (row.has_children, row.folded) {
            (false, _) => "  ",
            (true, true) => "> ",
            (true, false) => "v ",
        };
        let background = if selected {
            SELECTED
        } else if hovered {
            HOVERED
        } else {
            BACKGROUND
        };
        div()
            .id(("row", node as usize))
            .h(px(ROW))
            .pl(px(4. + INDENT * row.depth as f32))
            .flex()
            .flex_row()
            .items_center()
            .whitespace_nowrap()
            .bg(rgb(background))
            .text_color(rgb(colour))
            .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                if *over {
                    this.model.hover(Some(node));
                } else if this.model.hovered() == Some(node) {
                    this.model.hover(None);
                }
                this.publish();
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select(Some(node));
                window.focus(&this.tree_focus, cx);
                this.publish();
                cx.notify();
            }))
            .child(
                div()
                    .id(("arrow", node as usize))
                    .text_color(rgb(DIM))
                    .child(arrow)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.model.tree.toggle(node);
                        cx.stop_propagation();
                        cx.notify();
                    })),
            )
            .child(row.text.clone())
    }

    fn tree_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let list = uniform_list(
            "tree",
            self.model.tree.rows().len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let rows = this.model.tree.rows();
                let rows: Vec<Row> =
                    rows[range.start.min(rows.len())..range.end.min(rows.len())].to_vec();
                rows.iter().map(|row| this.row(row, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.tree_scroll)
        .size_full();
        div()
            .id("tree-panel")
            .track_focus(&self.tree_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.tree_key(event, cx)))
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .border_r_1()
            .border_color(rgb(LINE))
            .child(list)
    }

    fn styles_panel(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let lines: Rc<Vec<StyleLine>> = Rc::new(self.model.styles());
        let count = lines.len();
        let list = uniform_list(
            "styles",
            count,
            cx.processor(move |_, range: std::ops::Range<usize>, _, _| {
                let shown = range.start.min(lines.len())..range.end.min(lines.len());
                lines[shown].iter().map(style_line).collect::<Vec<_>>()
            }),
        )
        .size_full();
        div().flex_1().min_h(px(0.)).w_full().child(list)
    }

    fn console_panel(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.model.console.lines().len();
        let list = uniform_list(
            "console",
            count,
            cx.processor(|this, range: std::ops::Range<usize>, _, _| {
                let lines = this.model.console.lines();
                let end = range.end.min(lines.len());
                let shown = range.start.min(end)..end;
                shown.map(|at| console_line(&lines[at])).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.console_scroll)
        .size_full();
        div().flex_1().min_h(px(0.)).w_full().child(list)
    }

    fn input_row(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let input = &self.model.input;
        let text = input.text();
        let at = text
            .char_indices()
            .nth(input.caret())
            .map_or(text.len(), |(at, _)| at);
        let focused = self.input_focus.is_focused(window);
        let caret = if focused { "|" } else { "" };
        div()
            .id("repl")
            .track_focus(&self.input_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.input_key(event, cx)))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                window.focus(&this.input_focus, cx);
            }))
            .h(px(ROW + 4.))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .border_t_1()
            .border_color(rgb(LINE))
            .bg(rgb(PANEL))
            .text_color(rgb(TEXT))
            .child(div().text_color(rgb(TAG)).child("> "))
            .child(format!("{}{caret}{}", &text[..at], &text[at..]))
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let picking = self.model.picking;
        div()
            .h(px(28.))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .id("pick")
                    .px_2()
                    .cursor_pointer()
                    .border_1()
                    .border_color(rgb(if picking { TAG } else { LINE }))
                    .text_color(rgb(if picking { TAG } else { TEXT }))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_picking(cx)))
                    .child("Pick element"),
            )
            .child(div().text_color(rgb(DIM)).child(self.title.clone()))
    }
}

impl Render for InspectorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let side = div()
            .w(px(340.))
            .h_full()
            .flex()
            .flex_col()
            .child(box_diagram(self.model.box_model()))
            .child(self.styles_panel(cx));
        let top = div()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .flex()
            .flex_row()
            .child(self.tree_panel(cx))
            .child(side);
        let console = div()
            .h(px(190.))
            .w_full()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(LINE))
            .child(self.console_panel(cx))
            .child(self.input_row(window, cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .font_family(monospace())
            .child(self.toolbar(cx))
            .child(top)
            .child(console)
    }
}

/// The font the lists are set in.
fn monospace() -> &'static str {
    if cfg!(windows) {
        "Consolas"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "monospace"
    }
}

fn style_line(line: &StyleLine) -> impl IntoElement + use<> {
    div()
        .h(px(ROW))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .px_2()
        .gap_2()
        .whitespace_nowrap()
        .overflow_hidden()
        .child(div().text_color(rgb(TAG)).child(format!("{}:", line.name)))
        .child(line.value.clone())
}

fn console_line(line: &Line) -> impl IntoElement + use<> {
    let (mark, colour) = match line.kind {
        LineKind::Page(Level::Error) | LineKind::Failure => ("x ", ERROR),
        LineKind::Page(Level::Warn) => ("! ", WARNING),
        LineKind::Page(Level::Debug) => ("  ", DIM),
        LineKind::Page(_) | LineKind::Output => ("  ", TEXT),
        LineKind::Input => ("> ", TAG),
    };
    div()
        .h(px(ROW))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .px_2()
        .whitespace_nowrap()
        .overflow_hidden()
        .text_color(rgb(colour))
        .child(mark)
        .child(div().flex_1().overflow_hidden().child(line.text.clone()))
        .child(div().text_color(rgb(DIM)).child(line.origin.clone()))
}

/// The number DevTools writes for a width: nothing for zero.
fn width_text(width: f32) -> String {
    if width == 0. {
        "-".to_owned()
    } else if width.fract() == 0. {
        format!("{width:.0}")
    } else {
        format!("{width:.1}")
    }
}

/// One box of the diagram, with its four widths around what it holds. A side with no width shows
/// the style's own words (`auto`, `10%`).
fn nested(
    label: &str,
    colour: u32,
    (edges, words): (&Edges, &Sides<String>),
    inner: impl IntoElement,
) -> gpui::Div {
    let number = |width: Option<f32>, word: &str| {
        div().child(width.map_or_else(|| word.to_owned(), width_text))
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .p_1()
        .bg(rgb(colour))
        .text_color(rgb(0x0000_0000))
        .child(div().w_full().text_size(px(10.)).child(label.to_owned()))
        .child(number(edges.top, &words.top))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(number(edges.left, &words.left))
                .child(inner)
                .child(number(edges.right, &words.right)),
        )
        .child(number(edges.bottom, &words.bottom))
}

/// The box model of the selected node as DevTools draws it: margin, border, padding and content,
/// one inside the other.
fn box_diagram(model: Option<BoxModel>) -> impl IntoElement {
    let panel = div()
        .w_full()
        .flex()
        .justify_center()
        .py_2()
        .border_b_1()
        .border_color(rgb(LINE));
    let Some(model) = model else {
        return panel.text_color(rgb(DIM)).child("No box for this node");
    };
    // A padding only the layout can resolve leaves the content's size unknown.
    let size = model.content_box().map_or_else(
        || "?".to_owned(),
        |content| {
            let (width, height) = (width_text(content.width), width_text(content.height));
            format!("{width} x {height}")
        },
    );
    let content = div()
        .px_2()
        .bg(rgb(0x006f_a8dc))
        .text_color(rgb(0x0000_0000))
        .child(size);
    let [margin_words, border_words, padding_words] = &model.words;
    let padding = nested(
        "padding",
        0x0093_c47d,
        (&model.padding, padding_words),
        content,
    );
    let border = nested(
        "border",
        0x00ff_e599,
        (&model.border, border_words),
        padding,
    );
    panel.child(nested(
        "margin",
        0x00f6_b26b,
        (&model.margin, margin_words),
        border,
    ))
}
