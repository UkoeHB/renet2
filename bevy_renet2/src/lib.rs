#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "netcode")]
pub mod netcode;

#[cfg(feature = "steam")]
pub mod steam;

mod renet2;
mod run_conditions;

pub mod prelude {
    pub use crate::renet2::*;
    pub use crate::run_conditions::*;
}
