//! ROS 2 messages without generated code: definitions are read from the `.msg` files of the installed packages at
//! runtime, and serialized messages (CDR, as every RMW sends them) are decoded into a [`Value`] tree. A new message
//! type in the firmware needs no rebuild of archon.

pub mod cdr;
pub mod registry;
pub mod schema;
pub mod value;

pub use registry::Registry;
pub use schema::TypeName;
pub use value::Value;
