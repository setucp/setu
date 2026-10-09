//! Shared config, sealing, ids and error envelope.

pub mod config;
pub mod error;
pub mod ids;
pub mod seal;

pub use config::{Config, ConfigError, SecretKey};
pub use error::{AppError, ErrorCode};
pub use ids::{new_id, short_id};
pub use seal::{SealError, Sealer};
