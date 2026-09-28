# fork 部署补充

本文只记录本 fork 相对上游 [`deploy/README.md`](../../deploy/README.md) 的差异。合并上游时
`deploy/README.md` 直接取上游版本。89 生产环境的完整步骤见 [`deploy/89.md`](../../deploy/89.md)。

## 额外文件

| 文件 | 用途 |
| --- | --- |
| `deploy/compose.89.yaml` | 本 fork 在 89 与 sub2api 同机部署的覆盖文件 |
| `deploy/compose.gate.yaml`、`deploy/gate/nginx.conf` | 固定入口 cpr-gate（`cpr-green` 是它的网络别名），蓝绿槽位在其后切换 |
| `deploy/rollout.py` | 蓝绿发版、迁移清单比对、带迁移发版的备份校验（`--allow-additive-migrations --db-backup`） |
| `deploy/cloud-mint/`、`deploy/compose.ticket-login.yaml` | turn-state 云端打票 relay、票据登录服务 |

## 构建类型

构建时传 `CPR_BUILD_TYPE=fork`，不要用 `release` / `experimental`：上游的系统更新器对这两个值开放在线更新，
默认拉取 `zyycn/codex-proxy-rs`，在管理端点一下就会用上游原版覆盖 fork。`fork` 构建的在线更新返回
「在线更新需要官方发布构建」。

## 回退与升级注意

- 若启用过「固定自身 state」实验开关，回退到不支持该功能的旧版本前，先在当前版本关闭相关账号的开关并保存。
  旧版凭据解析不认识新增配置字段；原始 state 只保存在进程内，重启后会重新捕获。
- 带迁移的发版后旧镜像无法再启动（sqlx 拒绝库里存在它不认识的迁移），回滚只能用发版前的 `pg_dump` 恢复。
- 同一大版本内才支持在线升级；跨大版本使用全新 `.runtime/` 数据目录重新部署，并重新导入或授权账号与 Key。
