-- fork 独立号段(9xxx)：为 9006 在全新库上的执行铺路，线上库是空操作。
-- 上游 0028 删除了 model_requests.provider_account_id（历史表只留 provider_account_ref 快照），
-- 而 fork 已冻结的 9006 仍按该列建索引。版本号排序下全新库先跑 0001–0033 再跑 9001–9006，
-- 9006 会因列不存在而失败。此处只在 9006 尚未应用（全新库）时补一个空的同名列，
-- 让 9006 原样执行；随后 9007 再把列连同索引删掉。生产库 9006 已应用，本迁移什么也不做。
do $$
begin
  if not exists (select 1 from _sqlx_migrations where version = 9006 and success) then
    alter table model_requests add column if not exists provider_account_id text;
  end if;
end
$$;
