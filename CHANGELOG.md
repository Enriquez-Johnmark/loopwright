# Changelog

Every release, newest first, with what changed and what a product must do to upgrade. Versions follow `AGENTS.md`: while at `0.x`, a breaking change bumps the minor number.

## v0.1.0 (2026-10-24)

First release as its own repository. The engine is unchanged from the version built inside Kompleto, except:

- **Crate:** `loopwright`; paths that were `crate::engine::…` in Kompleto are `loopwright::…`.
- **Migrations:** the engine's five tables (`outbox_messages`, `jobs`, `audit_events`, `llm_calls`, `stored_files`) ship as one migration, exposed as `loopwright::MIGRATOR`. The schema is identical to Kompleto's, so Kompleto keeps its own migrations.
- **`id_type!`** is exported, so products define their own typed IDs with it.
- **Renamed:** the crash-test switch is `LOOPWRIGHT_FAILPOINT` (was `KOMPLETO_FAILPOINT`), and Mailpit's idempotency header is `X-Loopwright-Idempotency-Key`.

**Upgrading from the in-repository engine:** depend on the `v0.1.0` tag, replace `mod engine` with `pub use loopwright as engine;`, forward the `failpoints` feature to `loopwright/failpoints`, and rename `KOMPLETO_FAILPOINT` in crash tests.
