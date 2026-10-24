# Changelog

Every release, newest first, with what changed and what a product must do to upgrade. Versions follow `AGENTS.md`: while at `0.x`, a breaking change bumps the minor number.

## v0.3.0 (2026-10-24)

Two new modules, moved from Kompleto because a second product (Balanse) needs them. Nothing existing changed, so upgrading needs no code changes.

- **`accounts`:** an account per tenant with an owner and up to 5 staff; one-time login links (15 minutes) and sessions (30 days), with hourly limits; operator-approved sign-up requests; closing an account, reopening it within 7 days, and erasing it after. The product words the emails (`Logins::send_link` takes a function that writes the login email) and keeps its own pages and routes. A login email always goes to the member's own address, expires with its link, and is cancelled if a pre-send check refuses it.
- **`sheets`:** `read_csv` (UTF-8 or Windows-1252, rows numbered as a spreadsheet shows them) and `XlsxReader`, which parses Excel in a child process with no environment, memory and CPU limits, a timeout, bounded output and at most two running at once. Excel dates read as ISO dates. Both stop one row past `MAX_ROWS` (500) and refuse files over 2 MB.
- **Migration `20261024120000_accounts`:** new tables `accounts`, `account_members`, `account_login_tokens`, `account_sessions` and `account_signup_requests`. Each is prefixed "account" so none clashes with a product's own tables (Kompleto's `login_tokens`, `sessions` and `account_requests` stay as they are).
- **New dependencies:** `calamine`, `csv`, `encoding_rs`, `rustix` (process limits); tokio's `process` and `sync` features.

**Upgrading from v0.2.0:** depend on v0.3.0 by the commit its tag points to. To use `accounts`, point your per-tenant tables at `accounts (tenant_id)` with `on delete cascade`, so `Accounts::erase_due` removes your rows with the account's. To use `XlsxReader`, make your binary call `nudgewright::sheets::run_child()` first thing in `main` when its first argument is `nudgewright::sheets::CHILD_COMMAND`.

## v0.2.0 (2026-10-24)

**Renamed from Loopwright to Nudgewright**, because a US trademark application for "LOOPWRIGHT" (serial 99942826, filed 2026-07-15) covers closely related software. The engine's behaviour and schema are unchanged.

- **Crate:** `nudgewright` (was `loopwright`); paths are `nudgewright::…`.
- **Repository:** github.com/Enriquez-Johnmark/nudgewright (GitHub redirects the old address).
- **Renamed:** the crash-test switch is `NUDGEWRIGHT_FAILPOINT` (was `LOOPWRIGHT_FAILPOINT`), and Mailpit's idempotency header is `X-Nudgewright-Idempotency-Key`.
- **Migrations:** unchanged. The v0.1.0 migration still says "Loopwright's tables" in its first comment; released migrations are never edited, and its checksum stays the same, so databases that already ran it are unaffected.
- **Development database:** `nudgewright` (was `loopwright`).
- **Docs:** products pin a release by the commit its tag points to, so a moved tag can never change their build; outside contributions need a contributor agreement.

**Upgrading from v0.1.0:** depend on `nudgewright` at v0.2.0 by the commit its tag points to, rename `loopwright::` paths (or the re-export) to `nudgewright::`, rename a `loopwright/failpoints` feature to `nudgewright/failpoints`, and set `NUDGEWRIGHT_FAILPOINT` in crash tests.

## v0.1.0 (2026-10-23)

First release as its own repository. The engine is unchanged from the version built inside Kompleto, except:

- **Crate:** `loopwright`; paths that were `crate::engine::…` in Kompleto are `loopwright::…`.
- **Migrations:** the engine's five tables (`outbox_messages`, `jobs`, `audit_events`, `llm_calls`, `stored_files`) ship as one migration, exposed as `loopwright::MIGRATOR`. The schema is identical to Kompleto's, so Kompleto keeps its own migrations.
- **`id_type!`** is exported, so products define their own typed IDs with it.
- **Renamed:** the crash-test switch is `LOOPWRIGHT_FAILPOINT` (was `KOMPLETO_FAILPOINT`), and Mailpit's idempotency header is `X-Loopwright-Idempotency-Key`.

**Upgrading from the in-repository engine:** depend on v0.1.0 by the commit its tag points to (`rev = "649a774efb417bf579d288b7892cbf10b065ae2f"`), replace `mod engine` with `pub use loopwright as engine;`, forward the `failpoints` feature to `loopwright/failpoints`, and rename `KOMPLETO_FAILPOINT` in crash tests.
