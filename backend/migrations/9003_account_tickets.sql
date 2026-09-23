-- fork 子表：账号成本、到期时间与登录票据。只建自有表，不改上游表，避免合并冲突。
-- 票据（邮箱/密码/2FA 密钥）只以服务端 AES-256-GCM 密文落库，密钥不在数据库里。
create table account_tickets (
  provider_account_id text primary key
    references provider_accounts (id) on delete cascade,
  purchase_amount numeric(12, 2),
  purchase_currency text,
  purchased_at timestamptz,
  expires_at timestamptz,
  ticket_ciphertext bytea,
  ticket_hint text,
  ticket_updated_at timestamptz,
  updated_at timestamptz not null default now(),
  constraint account_tickets_amount_ck
    check (purchase_amount is null or purchase_amount >= 0),
  constraint account_tickets_currency_ck
    check (purchase_currency is null or purchase_currency in ('CNY', 'USD')),
  constraint account_tickets_amount_currency_ck
    check ((purchase_amount is null) = (purchase_currency is null)),
  constraint account_tickets_ticket_ck
    check ((ticket_ciphertext is null) = (ticket_hint is null))
);
