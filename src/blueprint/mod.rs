//! Blueprints: the XML that describes a mock world, and what it becomes.

pub mod model;
pub mod parse;
pub mod profiles;
pub mod xml;

pub use model::*;
pub use parse::load;
