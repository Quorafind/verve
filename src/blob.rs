//! `Blob`s as the runtime keeps them, for embedders that make or read one (a canvas's `toBlob`,
//! a dropped file) instead of reaching into how they are stored.

pub use crate::web::{blob_bytes, is_blob, new_blob};
