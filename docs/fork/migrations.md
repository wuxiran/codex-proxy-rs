# fork 迁移约定

上游规则见 [`backend/migrations/README.md`](../../backend/migrations/README.md)，合并上游时该文件直接取上游版本。

## 编号

- fork 自有迁移使用 `9xxx` 编号，与上游 `00xx` 永不相撞；sqlx 按「未应用即执行」处理，不要求版本递增，
  所以上游之后新增的 `00xx` 仍会在已应用 `9xxx` 的库上补跑。
- 当前 fork 迁移：`9000`（为 `9006` 补列的 shim）、`9001` 计费子表、`9002` 代理质量子表、`9003` 账号票据、
  `9004` 票据自动复活、`9005` 请求日志开关、`9006` 账号索引（已冻结）、`9007` 删 shim 列。
- `.frozen-sha256` 先列上游 `0001`–`0033`，再列 fork `9000`–`9007`，共 41 行；合并上游时保留两段。

## 字节谱系

生产库（89）在 2026-09-17 首次安装时，`0001`–`0011` 是以 **CRLF** 字节应用的；`_sqlx_migrations` 里记录的正是
这些字节的 checksum。因此本 fork 的这 11 个文件必须保持 CRLF 原样，`.frozen-sha256` 对应登记的是 CRLF 字节的哈希，
与上游（LF）不同，**合并上游时保留本 fork 的这 11 行**。`0012` 起与上游字节一致（LF）。根目录 `.gitattributes`
把 `*.sql` 标为 `-text`，任何平台检出都不做换行转换；不要去掉这条规则。

## 带迁移发版

新增迁移必须只增不改（建表、加带默认值的列），保证排空中的旧槽位在新 schema 上仍能服务；发版前在 89 上
`pg_dump -Fc` 并用 `pg_restore -f /dev/null` 完整读校验，再以 `rollout.py deploy --allow-additive-migrations
--db-backup <dump>` 发布。上线前可用线上 `pg_dump --schema-only` 加 `_sqlx_migrations` 数据在本地演练升级路径。

## 与上游 0021–0033 的顺序

- 生产库谱系是 `0001`–`0020` + `9001`–`9006`，再补跑上游 `0021`–`0033`；sqlx 只比对已应用迁移的 checksum、
  对未应用的按版本号顺序执行，不要求新版本号大于已应用的最大值，所以 `0021 < 9001` 不是问题
  （`tests/postgres/fork_request_log_settings.rs` 的 `upstream_migration_is_applied_after_existing_fork_migrations` 复现这条路径）。
- 上游 `0032` 重命名的表（`client_key_*`、`authorization_receipts`、`plugin_*_resources`、`request_analytics_hourly/daily_counts`）
  没有 fork 表或 fork SQL 引用；fork 自有表 `model_request_billing`、`outbound_proxy_quality`、`account_tickets` 不受影响。
- 上游 `0028` 删除了 `model_requests.provider_account_id` / `client_api_key_id`（历史表只保留 `*_ref` 快照，外键一并取消），
  fork 已冻结的 `9006` 却按 `provider_account_id` 建索引：
  - 线上库 `9006` 已应用，`0028` 删列时该索引随之消失；`0001` 的 `model_requests_account_ref_idx` 与 `0029` 的
    `model_requests_delivered_account` 覆盖同一访问路径，无需再补。
  - 全新库按版本号先跑完 `0033` 再跑 `9006`，列已不存在会直接失败。为此新增 `9000_account_id_shim_for_9006`
    （仅当 `9006` 尚未应用时补一个空的同名列）与 `9007_drop_account_id_shim`（删列，连带删掉 `9006` 建的空索引）。
    线上库两者都是空操作；全新库最终 schema 与线上一致。`9006` 字节保持冻结，不要改它。
- fork 查询里原来引用 `mr.provider_account_id` 的地方（票据已花费、账号 24h 报错数）改为 `provider_account_ref`。
