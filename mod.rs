//! The reusable engine. It must never depend on `crate::kompleto`
//! (ADR 0001); `tests/engine_boundary.rs` enforces this.

pub mod audit;
pub mod clock;
pub mod ids;
pub mod jobs;
pub mod mail;
pub mod outbox;
