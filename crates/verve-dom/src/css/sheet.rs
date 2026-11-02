//! Stylesheets: a `<style>`'s text read into rules, at-rules and their nesting.

use std::collections::HashMap;
use std::sync::Arc;

use super::animate::Keyframes;
use super::media::MediaQuery;
use super::selector::{Pseudo, SelectorList};
use super::syntax::{matching_brace, split_outside, strip_comments};

/// One declaration of a rule or an inline style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

/// `color: red; padding: 4px !important` as declarations, `;` inside parentheses and quotes
/// (`url(data:…;base64,…)`, `content: ";"`) left alone.
pub fn declarations(text: &str) -> Vec<Declaration> {
    split_outside(text, ';')
        .into_iter()
        .filter_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            let name = name.trim();
            let name = if name.starts_with("--") {
                name.to_owned()
            } else {
                name.to_ascii_lowercase()
            };
            let mut value = value.trim();
            let mut important = false;
            if let Some(at) = value.to_ascii_lowercase().rfind("!important")
                && value[at + "!important".len()..].trim().is_empty()
            {
                important = true;
                value = value[..at].trim();
            }
            (!name.is_empty() && (!value.is_empty() || name.starts_with("--"))).then(|| {
                Declaration {
                    name,
                    value: value.to_owned(),
                    important,
                }
            })
        })
        .collect()
}

/// The place of a rule that is in no layer: it beats every layer of its importance.
pub(super) const LAYERLESS: u32 = u32::MAX;

/// A style rule of a sheet, with the media queries and the layer it is in.
#[derive(Clone, Debug)]
pub(super) struct Rule {
    pub(super) selectors: SelectorList,
    pub(super) declarations: Vec<Declaration>,
    pub(super) media: Vec<MediaQuery>,
    /// The `@layer` it is in, as a place in its sheet's [`Sheet::layers`]; [`LAYERLESS`] when it
    /// is in none.
    pub(super) layer: u32,
    /// Whether a selector of it says `:hover` (or `:active`): the nodes it would change are
    /// the ones a drawer follows the pointer on.
    pub(super) pointer: bool,
}

/// One stylesheet: a `<style>`'s text, an adopted sheet, the embedder's own.
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub(super) rules: Vec<Rule>,
    /// Every layer it declares, by its full name (`a`, then `a.b`), in the order it declares
    /// them. A layer written without a name is `<n>`, `n` being its number in the sheet.
    pub(super) layers: Vec<String>,
    /// Every `@keyframes` it declares, by name.
    pub(super) keyframes: HashMap<String, Arc<Keyframes>>,
    /// The text it was read from, comments and all: what the CSSOM shows.
    source: String,
}

impl Sheet {
    pub fn parse(text: &str) -> Self {
        let source = text.to_owned();
        let mut parser = SheetParser::default();
        parser.block(&strip_comments(text), &[], "");
        Self {
            rules: parser.rules,
            layers: parser.layers,
            keyframes: parser.keyframes,
            source,
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// The top-level rules of a stylesheet as their own texts: what a CSSOM's `cssRules` lists.
pub fn split_rules(css: &str) -> Vec<String> {
    let text = strip_comments(css);
    let mut out = Vec::new();
    let mut rest = text.as_str();
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            break;
        }
        // A statement at-rule: `@import …;`, `@charset …;`.
        if trimmed.starts_with('@') {
            let semi = trimmed.find(';');
            let brace = trimmed.find('{');
            if let Some(semi) = semi
                && brace.is_none_or(|b| semi < b)
            {
                out.push(trimmed[..=semi].trim().to_owned());
                rest = &trimmed[semi + 1..];
                continue;
            }
        }
        let Some(open) = trimmed.find('{') else { break };
        let close = matching_brace(trimmed, open).unwrap_or(trimmed.len() - 1);
        out.push(trimmed[..=close.min(trimmed.len() - 1)].trim().to_owned());
        rest = trimmed.get(close + 1..).unwrap_or("");
    }
    out
}

/// A sheet read rule by rule, keeping the layers it declares in the order it meets them.
#[derive(Default)]
struct SheetParser {
    rules: Vec<Rule>,
    layers: Vec<String>,
    keyframes: HashMap<String, Arc<Keyframes>>,
    anonymous: usize,
}

impl SheetParser {
    /// Reads the rules of `text`, which is inside the media queries `media` and the layer
    /// `path` (`"a.b."`, a prefix of the names of what is inside it).
    fn block(&mut self, text: &str, media: &[MediaQuery], path: &str) {
        let mut rest = text;
        loop {
            let trimmed = rest.trim_start();
            if trimmed.is_empty() {
                break;
            }
            if let Some(end) = self.statement(trimmed, path) {
                rest = &trimmed[end + 1..];
                continue;
            }
            let Some(open) = trimmed.find('{') else { break };
            let prelude = trimmed[..open].trim();
            let close = matching_brace(trimmed, open).unwrap_or(trimmed.len());
            let body = &trimmed[open + 1..close.min(trimmed.len())];
            match prelude.strip_prefix('@') {
                Some(at_rule) => self.at_rule(at_rule, body, media, path),
                None => self.style_rule(prelude, body, media, path),
            }
            rest = trimmed.get(close + 1..).unwrap_or("");
        }
    }

    /// A statement at-rule, which ends at its `;`: `@layer a, b;` names layers without rules,
    /// `@import …;` and the rest are dropped. Answers where the `;` is, `None` when `text` does
    /// not start with one.
    fn statement(&mut self, text: &str, path: &str) -> Option<usize> {
        if !text.starts_with('@') {
            return None;
        }
        let semi = text.find(';')?;
        if text.find('{').is_some_and(|brace| brace < semi) {
            return None;
        }
        let (name, names) = at_rule_parts(&text[1..semi]);
        if name.eq_ignore_ascii_case("layer") {
            for name in split_outside(names, ',') {
                let name = name.trim();
                if !name.is_empty() {
                    self.declare(&format!("{path}{name}"));
                }
            }
        }
        Some(semi)
    }

    fn at_rule(&mut self, at_rule: &str, body: &str, media: &[MediaQuery], path: &str) {
        let (name, condition) = at_rule_parts(at_rule);
        match name.to_ascii_lowercase().as_str() {
            "media" => {
                let mut inner = media.to_vec();
                inner.push(MediaQuery::new(condition));
                self.block(body, &inner, path);
            }
            "layer" => {
                let named = condition.trim();
                let name = if named.is_empty() {
                    self.anonymous += 1;
                    format!("<{}>", self.anonymous)
                } else {
                    named.to_owned()
                };
                let inside = format!("{path}{name}");
                self.declare(&inside);
                self.block(body, media, &format!("{inside}."));
            }
            "keyframes" | "-webkit-keyframes" => {
                let name = condition.trim().trim_matches(['"', '\'']);
                if !name.is_empty() {
                    self.keyframes
                        .insert(name.to_owned(), Arc::new(Keyframes::parse(body)));
                }
            }
            // What it supports is what it was given; a document block is all documents.
            "supports" | "document" | "-moz-document" | "scope" | "starting-style" => {
                self.block(body, media, path)
            }
            _ => {}
        }
    }

    fn style_rule(&mut self, prelude: &str, body: &str, media: &[MediaQuery], path: &str) {
        let Ok(selectors) = SelectorList::parse(prelude) else {
            return;
        };
        let pointer = selectors.says(&|p| matches!(p, Pseudo::Hover | Pseudo::Active));
        let layer = match path.strip_suffix('.') {
            Some(name) => self.declare(name),
            None => LAYERLESS,
        };
        self.rules.push(Rule {
            pointer,
            selectors,
            declarations: declarations(body),
            media: media.to_vec(),
            layer,
        });
    }

    /// The place of the layer `name` (`"a.b"`) among the sheet's, declared now when nothing
    /// declared it before. `a.b` declares `a` too, before it: a layer is declared with
    /// everything it is inside.
    fn declare(&mut self, name: &str) -> u32 {
        let mut place = LAYERLESS;
        for (at, _) in name.match_indices('.').chain([(name.len(), "")]) {
            let prefix = &name[..at];
            place = match self.layers.iter().position(|layer| layer == prefix) {
                Some(found) => found as u32,
                None => {
                    self.layers.push(prefix.to_owned());
                    self.layers.len() as u32 - 1
                }
            };
        }
        place
    }
}

/// An at-rule's name and what follows it: `layer a, b` is `("layer", " a, b")`.
fn at_rule_parts(at_rule: &str) -> (&str, &str) {
    let end = at_rule
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(at_rule.len());
    (&at_rule[..end], &at_rule[end..])
}
