-- A message's deadline normally applies until it starts to send: approved
-- but held past it, it expires unsent. A product may instead set a deadline
-- only for approval: once approved, the message waits as long as it must.
alter table outbox_messages add column deadline_for_approval_only boolean not null default false;
