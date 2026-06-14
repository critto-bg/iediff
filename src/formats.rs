//! Parsers for individual Infinity Engine resource formats.
//!
//! Each submodule handles one format and owns its own error type. Shared
//! building blocks (the byte [`Reader`](crate::Reader), the [`Truncated`](crate::Truncated)
//! error, and small parsing helpers) live in the crate root.

pub mod bif;
pub mod ids;
pub mod key;
