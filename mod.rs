//! The reusable engine. It must never depend on `crate::kompleto`
//! (ADR 0001); `tests/engine_boundary.rs` enforces this.

pub mod audit;
pub mod clock;
pub mod cost_log;
pub mod files;
pub mod ids;
pub mod inbound;
pub mod jobs;
pub mod llm;
pub mod mail;
pub mod outbox;
pub mod scan;
pub mod signed;
pub mod tenants;
pub mod webhooks;
