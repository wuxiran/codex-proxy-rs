-- fork 子表：票据自动复活的尝试计数。只改 fork 自有的 account_tickets（行数很少），不动上游表。
-- 令牌失效时按票据自动重新登录，最多 3 次；成功或更换票据后清零。
alter table account_tickets
  add column auto_revive_attempts integer not null default 0,
  add column auto_revive_last_at timestamptz,
  add column auto_revive_last_error text,
  add constraint account_tickets_auto_revive_attempts_ck check (auto_revive_attempts >= 0);
