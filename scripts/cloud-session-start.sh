#!/usr/bin/env bash
# SessionStart hook: prepares a Claude Code on the web session (Postgres,
# pinned Rust, sqlx-cli, migrations). Does nothing on a developer's own
# machine.
set -uo pipefail
[ -n "${CLAUDE_CODE_REMOTE:-}" ] || exit 0
cd "${CLAUDE_PROJECT_DIR:-.}" || exit 0

log() { echo "cloud-session-start: $*" >&2; }

# Installs the toolchain pinned in rust-toolchain.toml, if rustup is present.
command -v rustup >/dev/null && rustup toolchain install >/dev/null 2>&1

[ -f .env ] || cp .env.example .env

# The cloud VM has Docker installed but doesn't always start the daemon.
if ! docker info >/dev/null 2>&1; then
    nohup dockerd >/tmp/dockerd.log 2>&1 &
    for _ in $(seq 1 30); do docker info >/dev/null 2>&1 && break; sleep 1; done
fi

# When another project in this session already runs Postgres on port 5432
# (e.g. Kompleto's), use it: just make sure the loopwright database exists.
if (exec 3<>/dev/tcp/127.0.0.1/5432) 2>/dev/null; then
    container=$(docker ps --filter publish=5432 --format '{{.Names}}' | head -1)
    [ -n "$container" ] && docker exec "$container" \
        psql -U kompleto -d postgres -qc "create database loopwright" >/dev/null 2>&1
else
    # Docker Hub rate-limits anonymous pulls (429); mirror.gcr.io serves the
    # same official images.
    docker image inspect postgres:17 >/dev/null 2>&1 || docker pull -q postgres:17 >/dev/null 2>&1 || {
        docker pull -q mirror.gcr.io/library/postgres:17 >/dev/null 2>&1 \
            && docker tag mirror.gcr.io/library/postgres:17 postgres:17
    }
    if ! docker compose up -d --wait >/dev/null 2>&1; then
        log "docker compose failed; database tests will not run"
        exit 0
    fi
fi

# Slow steps run in the background so the session isn't held up.
(
    command -v sqlx >/dev/null || cargo install sqlx-cli --version '^0.9' \
        --no-default-features --features postgres,rustls --locked \
        || log "installing sqlx-cli failed"
    set -a; . ./.env; set +a
    sqlx migrate run || log "migrations failed"
) >/tmp/cloud-session-start.log 2>&1 &
exit 0
