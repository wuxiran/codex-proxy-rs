-- fork 独立号段(9xxx)：收回 9000 为 9006 临时补的列。
-- 全新库：9006 已在 shim 列上建好空索引，删列会连带删掉该索引，schema 与线上一致。
-- 线上库：0028 早已删掉该列，9006 的索引随之消失（account_ref_idx / 0029 delivered_account 已覆盖），此处空操作。
alter table model_requests drop column if exists provider_account_id;
