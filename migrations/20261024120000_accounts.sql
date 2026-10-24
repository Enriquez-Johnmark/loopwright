-- Accounts (v0.3.0): who may log in to each tenant, and the account's life
-- from sign-up to erasure. A product points its own per-tenant tables at
-- accounts (tenant_id) with "on delete cascade", so erasing an account
-- removes the product's rows too. Every table is prefixed "account" so
-- none clashes with a product's own tables.

create table accounts (
    tenant_id    uuid primary key,
    -- The name the account's messages are signed with.
    name         text not null,
    created_at   timestamptz not null,
    closed_at    timestamptz,
    -- A closed account is erased at this moment unless it is reopened.
    erase_after  timestamptz,
    check ((closed_at is null) = (erase_after is null))
);

-- The people who log in to an account: one owner and some staff.
create table account_members (
    id          uuid primary key,
    tenant_id   uuid not null references accounts (tenant_id) on delete cascade,
    email       text not null,
    role        text not null check (role in ('owner', 'staff')),
    created_at  timestamptz not null,
    unique (id, tenant_id)
);

-- An address logs in to one account only.
create unique index account_members_email on account_members (lower(email));
create unique index account_members_one_owner on account_members (tenant_id) where role = 'owner';

-- Login links and sessions. These tables hold only SHA-256 hashes of the
-- secrets. A session's secret is stored nowhere else; a link's also sits in
-- its login email in the outbox, where it works once, for 15 minutes. Both
-- belong to a person and go with them.
create table account_login_tokens (
    token_hash  bytea primary key,
    tenant_id   uuid not null,
    member_id   uuid not null,
    created_at  timestamptz not null,
    expires_at  timestamptz not null,
    used_at     timestamptz,
    foreign key (member_id, tenant_id) references account_members (id, tenant_id) on delete cascade
);

create index account_login_tokens_member on account_login_tokens (member_id, created_at);

create table account_sessions (
    session_hash  bytea primary key,
    tenant_id     uuid not null,
    member_id     uuid not null,
    created_at    timestamptz not null,
    expires_at    timestamptz not null,
    foreign key (member_id, tenant_id) references account_members (id, tenant_id) on delete cascade
);

create index account_sessions_member on account_sessions (member_id);

-- Requests for an account from a public sign-up page. Not owned by any
-- account: only the operator reads them. A request is deleted once it is
-- approved or declined, or after 30 days.
create table account_signup_requests (
    id           uuid primary key,
    name         text not null,
    email        text not null,
    -- When the requester accepted the product's terms.
    accepted_at  timestamptz not null,
    created_at   timestamptz not null
);

create unique index account_signup_requests_email on account_signup_requests (lower(email));
