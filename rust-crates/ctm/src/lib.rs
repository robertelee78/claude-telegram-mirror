//! Claude Telegram Mirror — library re-exports.
//!
//! H2.4: This crate is intentionally both a binary (`main.rs`) and a library.
//! Downstream Rust consumers can depend on this crate and access any public
//! module, e.g. `use ctm::session::SessionManager`.

pub mod bot;
pub mod colors;
pub mod config;
pub mod daemon;
pub mod doctor;
pub mod error;
pub mod formatting;
pub mod hook;
pub mod injector;
pub mod installer;
pub mod service;
pub mod session;
pub mod setup;
pub mod socket;
pub mod summarize;
pub mod types;
