pub mod config;
pub mod event;
pub mod ipc;
pub mod paths;
pub mod ports;
pub mod site;
pub mod wpcli;

pub use event::{random_token, Event};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
