-- no-transaction
-- 账号列表的 24h 报错数 / 票据已花费按 provider_account_id + started_at 过滤，
-- 原只有单列索引（带 started_at 的复合索引建在 provider_account_ref 上用不上），
-- 每次列表都要回表扫账号全历史。concurrently 建索引不锁写，需免事务迁移。
create index concurrently if not exists model_requests_account_started_idx
    on model_requests (provider_account_id, started_at desc, id desc)
    where provider_account_id is not null;
