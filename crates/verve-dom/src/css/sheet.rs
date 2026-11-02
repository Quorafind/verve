//! Stylesheets: a `<style>`'s text read into rules, at-rules and their nesting.

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

#[derive(Clone, Debug)]
pub(super) struct Rule {
    pub(super) selectors: SelectorList,
    pub(super) declarations: Vec<Declaration>,
    pub(super) media: Vec<MediaQuery>,
    /// Whether a selector of it says `:hover` (or `:active`): the nodes it would change are
    /// the ones a drawer follows the pointer on.
    pub(super) pointer: bool,
}

/// One stylesheet: a `<style>`'s text, an adopted sheet, the embedder's own.
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub(super) rules: Vec<Rule>,
    /// The text it was read from, comments and all: what the CSSOM shows.
    source: String,
}

impl Sheet {
    pub fn parse(text: &str) -> Self {
        let source = text.to_owned();
        let text = strip_comments(text);
        let mut rules = Vec::new();
        parse_block(&text, &[], &mut rules);
        Self { rules, source }
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

fn parse_block(text: &str, media: &[MediaQuery], rules: &mut Vec<Rule>) {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            break;
        }
        // Statement at-rules end at their `;`.
        if trimmed.starts_with('@') {
            let semi = trimmed.find(';');
            let brace = trimmed.find('{');
            if let Some(semi) = semi
                && brace.is_none_or(|b| semi < b)
            {
                rest = &trimmed[semi + 1..];
                continue;
            }
        }
        let Some(open) = trimmed.find('{') else { break };
        let prelude = trimmed[..open].trim();
        let close = matching_brace(trimmed, open).unwrap_or(trimmed.len());
        let body = &trimmed[open + 1..close.min(trimmed.len())];
        if let Some(at_rule) = prelude.strip_prefix('@') {
            let (name, condition) = at_rule
                .split_once(|c: char| c.is_whitespace() || c == '(')
                .map_or((at_rule, ""), |(n, _)| (n, &at_rule[n.len()..]));
            match name.to_ascii_lowercase().as_str() {
                "media" => {
                    let mut inner = media.to_vec();
                    inner.push(MediaQuery::new(condition));
                    parse_block(body, &inner, rules);
                }
                // What it supports is what it was given; layers are read in order.
                "supports" | "layer" | "document" | "-moz-document" | "scope"
                | "starting-style" => parse_block(body, media, rules),
                _ => {}
            }
        } else if let Ok(selectors) = SelectorList::parse(prelude) {
            let pointer = selectors.says(&|p| matches!(p, Pseudo::Hover | Pseudo::Active));
            rules.push(Rule {
                pointer,
                selectors,
                declarations: declarations(body),
                media: media.to_vec(),
            });
        }
        rest = trimmed.get(close + 1..).unwrap_or("");
    }
}
