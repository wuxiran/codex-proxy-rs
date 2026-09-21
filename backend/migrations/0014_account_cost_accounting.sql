-- 购买记录独立于账号生命周期：account_ref 不设外键，账号删除后成本仍可核算。
-- retired_at 只由管理员手动标记；系统不会因为额度用满或凭据失效而自动下线。
create table account_purchases (
    account_ref text primary key check (char_length(account_ref) between 1 and 128),
    name_snapshot text not null,
    email_snapshot text,
    price numeric(12, 2) check (price is null or price >= 0),
    purchased_at timestamptz not null,
    retired_at timestamptz,
    note text check (note is null or char_length(note) <= 512),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index account_purchases_purchased_at_idx on account_purchases (purchased_at);
create index account_purchases_retired_idx on account_purchases (account_ref) where retired_at is not null;

-- 请求日志只保留有限天数；每日跑出的金额在保留期内反复重算并落在这里，过期后只读。
-- day 是东八区自然日。
create table account_cost_daily (
    day date not null,
    account_ref text not null,
    usage_usd numeric(20, 10) not null check (usage_usd >= 0),
    request_count bigint not null check (request_count >= 0),
    total_tokens bigint not null check (total_tokens >= 0),
    computed_at timestamptz not null default now(),
    primary key (day, account_ref)
);

create index account_cost_daily_account_idx on account_cost_daily (account_ref, day);
