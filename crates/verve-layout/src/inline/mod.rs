//! Inline layout: the text of a block, and the inline boxes in it, broken into lines.
//!
//! A block whose content is inline (text, `<span>`, `<br>`, `inline-block`, `<img>`) is an
//! *inline formatting context*. [`collect`] gathers its content into [`Ifc`] items (white space
//! collapsed, text shaped), `chunks` cuts it at the places a line may break, and `lines` fills
//! lines with the chunks, aligns them and puts each box at its height. The block's taffy node is a
//! leaf whose measure function is [`Ifc::measure`]; the lines at the width taffy settled on are
//! read afterwards for where everything is.

pub(crate) mod breaks;
