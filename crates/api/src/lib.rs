//! Control-plane HTTP API (spec §6, plan Task 7).
//!
//! [`state::AppState`] is shared by every handler; [`auth`] holds session
//! extraction, role checks and password hashing; [`rest`] assembles the
//! `/api/v1` router; [`testing::TestServer`] boots the whole stack against
//! a throwaway database for integration tests.

pub mod auth;
pub mod rest;
pub mod session_store;
pub mod state;
pub mod testing;

pub use state::AppState;
