//! utopia-core: the domain model, error types, and configuration.

pub mod config;
pub mod error;
pub mod models;

pub use error::{is_terminal, AppError, AppResult, Terminal};
