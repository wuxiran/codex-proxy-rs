# 合并上游流程

远端：`origin` = 本 fork，`upstream` = zyycn/codex-proxy-rs。最近一次整版合并：2026-09-28 合入上游 `3915aa25`（v3.17.0），
合并提交 `6d0ff951`。

## 1. 准备

```bash
git fetch upstream --prune
git worktree add -b merge/upstream-<版本>-<日期> .worktrees/upmerge origin/main
cd .worktrees/upmerge
python3 tools/fork_divergence.py            # 合并前记录侵入面
git merge --no-ff --no-commit upstream/main
```

## 2. 解决冲突

先读 [FORK_HOOKS_REGISTRY.md](FORK_HOOKS_REGISTRY.md)：

- A / B / D 类（一行钩子、接线、测试模块声明）：取上游版本，再补回登记表里的标记行。
- C 类（行为修改残留）：按登记表的「合并策略」逐处比对。
- 上游文档（`README.md`、`docs/*.md`、`deploy/README.md`、`backend/migrations/README.md`）直接取上游；fork 文档在 `docs/fork/`。
- `backend/migrations/.frozen-sha256`：保留上游 `00xx` 行 + fork `9xxx` 行；`0001`–`0011` 保持 fork 的 CRLF 哈希（见 [migrations.md](migrations.md)）。
- 上游改了 fork 依赖的接口时，改 fork 模块去适配上游新接口，不要让上游旧接口复活。

冲突多时可按 crate 分给多个并行任务（文件互不重叠、只改自己的文件、不跑编译），全部完成后统一编译。

## 3. 验证

- WSL：`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`。
- 全量测试：store 连临时 Postgres（`postgres:18-bookworm -c max_connections=1000`，`CPR_TEST_DATABASE_URL`），
  `cargo test -p gateway-store -- --test-threads=1`；其余 `cargo test --workspace --exclude gateway-store`。
  并发运行会让迁移 advisory lock 死锁，store 必须单线程。失败清单与合并前的 main 逐项对比（已知失败：
  3 个架构测试、3 个 store 测试、websocket 计时类、`idle_connection_reaches_the_official_limit…`）。
- 前端：`npx pnpm@<packageManager 版本> install --frozen-lockfile`、`typecheck`、`eslint`、`build`。
- 带迁移时：用线上 `pg_dump --schema-only` + `_sqlx_migrations` 数据在本地恢复，启动新镜像验证升级路径。

## 4. 发布

构建用 `CPR_BUILD_TYPE=fork`（见 [deploy.md](deploy.md)），按 `deploy/89.md` 蓝绿发布。
