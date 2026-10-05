//! The link to the server: the client of its observer, `mod-uniwow-observer`, which streams the
//! creatures, game objects and players of a zone of a map, read only; and a fake observer for the
//! tests.

pub mod client;
pub mod fake;
pub mod protocol;

pub use client::{Client, Error};

#[cfg(test)]
mod tests;
