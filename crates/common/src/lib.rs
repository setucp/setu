//! Shared config, sealing, ids, domain normalisation, validation and the
//! error envelope.

pub mod config;
pub mod domain;
pub mod error;
pub mod ids;
pub mod seal;
pub mod validate;

pub use config::{Config, ConfigError, Listen, SecretKey};
pub use error::{AppError, ErrorCode};
pub use ids::{is_short_id, new_id, short_id};
pub use seal::{SealError, Sealer};
