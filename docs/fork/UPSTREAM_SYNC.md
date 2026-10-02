# 合并上游流程

远端：`origin` = 本 fork，`upstream` = zyycn/codex-proxy-rs
当前上游基线为 v3.18.3（`9c106767`），同步以明确的稳定标签为目标

## 1. 准备

```bash
git fetch --no-tags upstream refs/tags/<版本>:refs/codex/upstream-<版本>
git worktree add -b sync/upstream-v<版本> ~/worktrees/cpr-upstream-<版本> origin/dev
cd ~/worktrees/cpr-upstream-<版本>
python3 tools/fork_divergence.py --base "$(git merge-base HEAD refs/codex/upstream-<版本>)"
git merge --no-ff --no-commit refs/codex/upstream-<版本>
```

工作树目录须尚未存在，构建产物与 `node_modules` 保持独立。同步分支从 `dev` 拉出，合并完成后合回 `dev`（见 [GIT_WORKFLOW.md](GIT_WORKFLOW.md)）

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
- 全量测试：连接独立 PostgreSQL 18 与 Redis，配置 `CPR_TEST_DATABASE_URL` / `CPR_TEST_REDIS_URL`
  （密码要求见迁移说明），执行 `RUST_MIN_STACK=16777216 cargo +1.97.0 test --workspace --no-fail-fast --locked -- --test-threads=1`
- Store 与插件 Runtime 的数据库测试都涉及迁移，使用单线程避免 advisory lock 死锁
  失败用例在合并前的同一基线、同样线程配置下复跑，逐项区分继承失败、时序波动与合并回归
  缺少测试库导致的跳过不算通过，不以历史失败数量代替当前证据
- 前端：`npx pnpm@<packageManager 版本> install --frozen-lockfile`、`typecheck`、`eslint`、`build`。
- 带迁移时：用线上 `pg_dump --schema-only` + `_sqlx_migrations` 数据在本地恢复，启动新镜像验证升级路径。

## 4. 发布

合回 `dev` 后按 [GIT_WORKFLOW.md](GIT_WORKFLOW.md#发版) 发版：fork 版本号前三段跟上游、第四段从 1 开始，
构建用 `CPR_BUILD_TYPE=fork`（见 [deploy.md](deploy.md)），按 `deploy/89.md` 蓝绿发布。
