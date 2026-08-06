//! Library crate. All testable logic lives here. The binary in
//! `src/main.rs` is a thin CLI wrapper.

#![deny(warnings)]

pub mod cli;
pub mod client;
pub mod compare;
pub mod config;
pub mod config_io;
pub mod dataset;
pub mod error;
pub mod report;
pub mod runner;
pub mod storage;
pub mod tui;
