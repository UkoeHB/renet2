#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(rustdoc::redundant_explicit_links)]
#![doc = include_str!("../README.md")]
#[allow(unused_imports)]
use crate as renet2_setup;

#[cfg(feature = "client")]
pub mod client;
pub mod common;
#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "client")]
pub use client::*;
pub use common::*;
#[cfg(feature = "server")]
pub use server::*;
