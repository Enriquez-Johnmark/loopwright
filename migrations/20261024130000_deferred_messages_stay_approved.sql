-- A message whose pre-send check couldn't run now stays 'approved' with a
-- retry time, so the deadline sweep and cancelling reach it. Messages
-- already deferred the old way were left 'failed' without ever starting to
-- send (a real attempt sets first_send_at); they become approved again.
update outbox_messages
set status = 'approved'
where status = 'failed' and first_send_at is null;
