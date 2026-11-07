//! The inspector that stands in for DevTools (P43): a GPUI window per page with the element tree,
//! the computed style and box model of the selected node, a highlight of it over the page, an
//! element picker, and the page's console with a REPL.
//!
//! The data is plain Rust, apart from any window: [`tree`] (the rows, folding, incremental
//! updates), [`styles`], [`boxmodel`], [`console`] (lines and the line editor) and [`model`]
//! (what one window knows of its page).

pub mod boxmodel;
pub mod console;
pub mod model;
pub mod overlay;
pub mod styles;
pub mod tree;
