# AGENTS.md

**Loopwright** is a reusable Rust engine for human-approved reminder loops: a product drafts a message, a person approves it, the outbox sends it exactly once (even across crashes), replies and bounces come back and stop the loop, and every step is audited and scoped to one tenant. It is AGPL-3.0-only.

Products depend on it by tag (`loopwright = { git = "https://github.com/Enriquez-Johnmark/loopwright", tag = "vX.Y.Z" }`). The first product is Kompleto (bookkeepers); Balanse, Kotasyon, Biyahe, Iskedyul and Resibo follow, each in its own repository.

## What belongs here

- **In the engine:** anything every product needs and that protects a guarantee: the outbox and approval, sending exactly once, mail providers, inbound replies and delivery events, encrypted file storage and virus scanning, the AI client and its cost log, jobs, the audit log, tenant erasure, the clock, signed links, typed IDs.
- **Not in the engine:** brand names, message wording and templates, schedules and deadlines, pages, a product's own tables. If only one product needs it, it stays in that product until a second one does.
- **The engine never names a product.** CI fails if `src/` or `migrations/` mention one.

## Rules

Each rule protects a guarantee products rely on. Where a tool can enforce a rule, the tool does.

- **Read the `rust-best-practices` skill** (`.claude/skills/rust-best-practices`) before writing or reviewing Rust.
- **Sending:** only `outbox` sends messages, and only an approved message whose content hash still matches what the approver saw, with an idempotency key recorded before the provider call. An outcome that is unknown (timeout, crash) is never retried on a provider that can't block duplicates.
- **Tenant isolation:** every function that reads or writes tenant data takes a `TenantId`, and every query filters on it. The exceptions are system-wide workers (the outbox sender, the job runner, sweeps), which still name the row's tenant in every update.
- **Time:** read time only through the injected `Clock`. Direct system-clock calls are banned with clippy `disallowed-methods`. UUIDv7 IDs embed wall-clock time; they are identifiers only, never used to decide anything about time.
- **Untrusted input:** model output, inbound email, webhooks and uploaded files are untrusted. Parse them into strict types, verify signatures before trusting a payload, and never echo them into commands or SQL.
- **Agent safety lives in code, not prompts.** Approval gates, idempotent sends and bounded loops are enforced by the engine, never left to a model's instructions.
- **Schema:** the engine's tables live in `migrations/` and never reference a product's tables. A change to them is a new migration, never an edit to a released one, and is listed in `CHANGELOG.md`.
- **Report test results only from commands actually run.**

## Versions

- Semantic versions, tagged `vX.Y.Z` on `main`. While the version is `0.x`, a breaking change bumps the minor number (`0.1` → `0.2`); anything else bumps the patch.
- Every release adds an entry to `CHANGELOG.md` saying what changed and what a product must do to upgrade.
- A product upgrades on its own schedule; an engine change never forces one product's work onto another.

## Workflow

The owner is the only developer: work directly on `main`. No branches, no pull requests.

1. **Test first:** write the test for one behaviour, see it fail, then make it pass.
2. **Small commits:** one behaviour (a test plus the code that makes it pass) or one small refactor or doc change, aiming for under ~150 changed lines excluding `.sqlx/` and `Cargo.lock`. Messages are imperative and describe the behaviour. Commit messages carry no AI attribution trailers such as "Co-Authored-By" (owner's choice).
3. **Before every commit:** `cargo fmt --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings` and `cargo test --all-features --locked` all pass. If SQL changed, run `cargo sqlx prepare -- --all-targets --all-features` (with `DATABASE_URL` set, see `.env.example`) and commit `.sqlx/`.
4. **Push** to `origin main` after each commit. If CI goes red, fixing it is the next commit.
5. **At the end of a piece of work,** run the `code-review` skill and fix findings in small commits.

## Development

- **Coding happens in Claude Code on the web.** `scripts/cloud-session-start.sh` runs at session start (via `.claude/settings.json`), brings up Postgres and the pinned Rust, and applies the migrations. On a local machine it does nothing.
- Locally: `docker compose up -d` starts Postgres 17; `cp .env.example .env` once (for `sqlx-cli`); `sqlx migrate run`; then `cargo test`.
- The send-exactly-once crash test needs a real program to kill, so it lives in each product (Kompleto: `tests/e1_crash.rs`). Products enable it with their own `failpoints` feature, which turns on `loopwright/failpoints`; the crash point is chosen with `LOOPWRIGHT_FAILPOINT`.

## Scope

Work only inside the approved scope. When something outside it looks worth doing, raise it as a proposal for the owner.
