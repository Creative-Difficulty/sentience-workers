//! Integration tests (single binary to keep build output small).
//!
//! DB tests need `DATABASE_URL` pointing at a Postgres loaded with the unidb schema; each test
//! clones it into its own database.

mod common;
mod edits;
mod handlers;
mod historical;
mod mapping;
mod media;
mod reactions;
mod users;
