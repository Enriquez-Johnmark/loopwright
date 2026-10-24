//! Nudgewright: a durable engine for human-approved reminder loops. A
//! product drafts a message; a person approves it; the outbox sends it
//! exactly once, even across crashes; replies and bounces come back and
//! stop the loop; every step is audited and scoped to one tenant.
//!
//! Products depend on a tagged version of this crate. The engine knows
//! nothing about any product: brand text, message templates and schedules
//! live in the product.

pub mod accounts;
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

/// The engine's database tables. A product runs these before its own
/// migrations, e.g. `nudgewright::MIGRATOR.run(&pool).await?`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
