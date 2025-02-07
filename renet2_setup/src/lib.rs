#![cfg_attr(docsrs, feature(doc_auto_cfg))]

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
