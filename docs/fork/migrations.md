# fork 迁移约定

上游规则见 [`backend/migrations/README.md`](../../backend/migrations/README.md)，合并上游时该文件直接取上游版本。

## 编号

- fork 自有迁移使用 `9xxx` 编号（`9001_model_request_billing` 起），与上游 `00xx` 永不相撞；sqlx 按「未应用即执行」
  处理，不要求版本递增，所以上游之后新增的 `00xx` 仍会在已应用 `9xxx` 的库上补跑。
- `.frozen-sha256` 先列上游 `0001`–`00xx`，再列 fork `9001`–`9xxx`；合并上游时保留两段。

## 字节谱系

生产库（89）在 2026-09-17 首次安装时，`0001`–`0011` 是以 **CRLF** 字节应用的；`_sqlx_migrations` 里记录的正是
这些字节的 checksum。因此本 fork 的这 11 个文件必须保持 CRLF 原样，`.frozen-sha256` 对应登记的是 CRLF 字节的哈希，
与上游（LF）不同，**合并上游时保留本 fork 的这 11 行**。`0012` 起与上游字节一致（LF）。根目录 `.gitattributes`
把 `*.sql` 标为 `-text`，任何平台检出都不做换行转换；不要去掉这条规则。

## 带迁移发版

新增迁移必须只增不改（建表、加带默认值的列），保证排空中的旧槽位在新 schema 上仍能服务；发版前在 89 上
`pg_dump -Fc` 并用 `pg_restore -f /dev/null` 完整读校验，再以 `rollout.py deploy --allow-additive-migrations
--db-backup <dump>` 发布。上线前可用线上 `pg_dump --schema-only` 加 `_sqlx_migrations` 数据在本地演练升级路径。
