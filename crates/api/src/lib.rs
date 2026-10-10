//! Control-plane HTTP API (spec §7, plan Tasks 7–9).
//!
//! [`state::AppState`] is shared by every handler; [`auth`] holds
//! sessions, the client IP, role checks, the `Origin` guard and password
//! hashing; [`rest`] assembles the router; [`listen`] binds the unix
//! socket or TCP listener; [`notifier`] wakes agent long polls; [`cli`]
//! backs `setu operator create` and `setu backup`;
//! [`testing::TestServer`] boots the whole stack against a throwaway
//! database for integration tests.

pub mod auth;
pub mod cli;
pub mod embed;
pub mod listen;
pub mod notifier;
pub mod rest;
pub mod self_backup;
pub mod state;
pub mod testing;

pub use state::AppState;
