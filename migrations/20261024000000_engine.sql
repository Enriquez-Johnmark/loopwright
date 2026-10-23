-- Loopwright's tables. Each is scoped by tenant_id and never references a
-- product's tables, so any product can run these next to its own. Every time
-- value is bound from the application's Clock; no column defaults to now().
--
-- Consolidated from the engine's first migrations (2026-10-21 to 2026-10-22),
-- written while it was built inside its first product; the schema is identical.

create table outbox_messages (
    id                  uuid primary key,
    tenant_id           uuid not null,
    status              text not null check (status in (
                            'draft', 'approved', 'sending', 'sent', 'failed',
                            'blocked', 'unconfirmed', 'expired', 'cancelled')),
    to_address          text not null,
    from_name           text not null,
    reply_to            text,
    subject             text not null,
    body_text           text not null,
    -- Hash of the exact content the approver saw.
    approved_hash       bytea,
    approved_at         timestamptz,
    approved_by         text,
    send_attempts       integer not null default 0,
    -- Set once an attempt's outcome was unknown (timeout, crash): the email
    -- may have gone out, so only a duplicate-blocking provider may retry.
    may_have_sent       boolean not null default false,
    first_send_at       timestamptz,
    send_started_at     timestamptz,
    retry_at            timestamptz,
    provider_message_id text,
    sent_at             timestamptz,
    last_error          text,
    created_at          timestamptz not null,
    updated_at          timestamptz not null,
    -- An unapproved draft expires at this moment.
    expires_at          timestamptz,
    check (status = 'draft' or status in ('expired', 'cancelled') or approved_hash is not null)
);

create index outbox_messages_tenant on outbox_messages (tenant_id, created_at);
create index outbox_messages_due on outbox_messages (approved_at) where status in ('approved', 'failed');
create index outbox_messages_sending on outbox_messages (send_started_at) where status = 'sending';
create index outbox_messages_expiring on outbox_messages (expires_at) where status = 'draft';
create index outbox_messages_first_send_at on outbox_messages (first_send_at) where first_send_at is not null;
create index outbox_messages_provider_message_id on outbox_messages (provider_message_id)
    where provider_message_id is not null;

create table jobs (
    id            uuid primary key,
    tenant_id     uuid,
    kind          text not null,
    payload       jsonb not null,
    status        text not null check (status in ('queued', 'done', 'dead')),
    run_at        timestamptz not null,
    locked_until  timestamptz,
    attempts      integer not null default 0,
    last_error    text,
    created_at    timestamptz not null,
    updated_at    timestamptz not null
);

create index jobs_ready on jobs (run_at) where status = 'queued';

create table audit_events (
    id            uuid primary key,
    tenant_id     uuid not null,
    at            timestamptz not null,
    actor         text not null,
    action        text not null,
    subject_type  text not null,
    subject_id    uuid not null,
    details       jsonb not null default '{}'
);

create index audit_events_tenant on audit_events (tenant_id, at);

-- The audit log is append-only, except that erasing a closed tenant deletes
-- that one tenant's entries: the transaction names the tenant in
-- `engine.erasing_tenant` first, and only its rows can go.
create function audit_events_append_only() returns trigger language plpgsql as $$
begin
    if tg_op = 'DELETE' and old.tenant_id::text = current_setting('engine.erasing_tenant', true) then
        return old;
    end if;
    raise exception 'audit_events is append-only';
end;
$$;

create trigger audit_events_no_update_or_delete
    before update or delete on audit_events
    for each row execute function audit_events_append_only();

create table llm_calls (
    id             uuid primary key,
    tenant_id      uuid not null,
    at             timestamptz not null,
    model          text not null,
    input_tokens   integer not null,
    output_tokens  integer not null,
    cost_nano_usd  bigint,
    request_id     text,
    outcome        text not null check (outcome in ('ok', 'invalid', 'failed'))
);

create index llm_calls_tenant on llm_calls (tenant_id, at);

create table stored_files (
    id           uuid primary key,
    tenant_id    uuid not null,
    size_bytes   integer not null,
    -- Null once the file is deleted: its key is gone with it.
    wrapped_key  bytea,
    created_at   timestamptz not null,
    delete_by    timestamptz not null,
    deleted_at   timestamptz,
    check ((deleted_at is null) = (wrapped_key is not null))
);

create index stored_files_due on stored_files (delete_by) where deleted_at is null;
create index stored_files_tenant on stored_files (tenant_id, created_at);
