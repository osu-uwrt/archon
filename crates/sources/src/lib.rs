//! Line sources and the supervisor that runs them.

pub mod process;
pub mod serial;
pub mod supervisor;

pub use supervisor::{Entry, Supervisor};
