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
