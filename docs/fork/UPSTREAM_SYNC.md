# 合并上游流程

远端：`origin` = 本 fork，`upstream` = zyycn/codex-proxy-rs
当前上游基线为 `ab8c61b1`（`v3.29.1` 之后 4 个提交，含工具链升级）。同步以明确的稳定标签为目标；
标签之后有必须带上的修复时，可以取 `upstream/main` 上的具体提交，并在本文与登记表写明哈希。

## 工具链

以上游仓库里声明的版本为准，合并后同步本机环境：

| 组件 | 版本 | 声明位置 |
| --- | --- | --- |
| Rust | 1.99.1 | `backend/rust-toolchain.toml`、`backend/Cargo.toml` 的 `rust-version` |
| Node | 26.12.0 | `deploy/Dockerfile` 的 `frontend-builder` 镜像（CI 用 `.github/scripts/toolchains.rb` 读取） |
| pnpm | 12.6.0 | `frontend/package.json` 的 `packageManager` |

## 1. 准备

```bash
git fetch --no-tags upstream main
git worktree add -b sync/upstream-<日期或版本> <工作树目录> origin/dev
cd <工作树目录>
python3 tools/fork_divergence.py --base "$(git merge-base HEAD upstream/main)"
git merge --no-ff --no-commit <上游提交或标签>
```

- 工作树目录须尚未存在，构建产物与 `node_modules` 保持独立。同步分支从 `dev` 拉出，合并完成后合回 `dev`（见 [GIT_WORKFLOW.md](GIT_WORKFLOW.md)）。
- 工作树建在哪一侧，git 操作就只在那一侧做：Windows 侧的工作树用 Git Bash，WSL 的 git 读不了它的 `.git` 指针；
  两侧都不要跑 `git worktree prune`，否则会把另一侧的工作树登记清掉。

## 2. 解决冲突

先读 [FORK_HOOKS_REGISTRY.md](FORK_HOOKS_REGISTRY.md)：

- A / B / D 类（一行钩子、接线、测试模块声明）：取上游版本，再补回登记表里的标记行。
- C 类（行为修改残留）：按登记表的「合并策略」逐处比对。
- 上游已有等价能力的 fork 改动放弃，用上游实现；登记表开头列了已放弃的清单，不要补回。
- 上游移动或重命名了文件（目录改 kebab-case、`accounts.rs` 拆成 `accounts/mod.rs`、测试目录改名）时，
  fork 自有文件跟着搬到新位置，旧路径删掉；登记表记当前路径。
- 上游文档（`README.md`、`docs/*.md`、`deploy/README.md`、`backend/migrations/README.md`、`release/notes.md`）直接取上游；fork 文档在 `docs/fork/`。
- `backend/migrations/.frozen-sha256`：保留上游 `00xx` 行 + fork `9xxx` 行；`0001`–`0011` 保持 fork 的 CRLF 哈希（见 [migrations.md](migrations.md)）。
- 上游改了 fork 依赖的接口时，改 fork 模块去适配上游新接口，不要让上游旧接口复活。
- fork 测试一律放 `tests/` 下的 `fork_*.rs`；上游测试文件里只留夹具与可见性放宽，`src` 不放 `#[cfg(test)]`。

冲突多时可按 crate 分给多个并行任务（文件互不重叠、只改自己的文件、不跑编译），全部完成后统一编译。

## 3. 验证

- WSL 跑后端测试前先 `unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY`：
  带着代理环境变量时 WebSocket 测试会以 `WrongHttpMethod` 失败。
- `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`。
- 全量测试：连接独立 PostgreSQL 18 与 Redis，配置 `CPR_TEST_DATABASE_URL` / `CPR_TEST_REDIS_URL`
  （密码要求见迁移说明），执行 `RUST_MIN_STACK=16777216 cargo test --workspace --no-fail-fast --locked -- --test-threads=1`
  （toolchain 文件已钉 1.99.1，不必再写 `+<版本>`）。
- Store 与插件 Runtime 的数据库测试都涉及迁移，使用单线程避免 advisory lock 死锁。
- 失败用例在合并前的同一基线、同样线程配置下复跑，逐项区分继承失败、时序波动与合并回归；
  缺少测试库导致的跳过不算通过，不以历史失败数量代替当前证据。
- 前端：`npx pnpm@12.6.0 install --frozen-lockfile`、`typecheck`、`eslint`、`build`。
- 带迁移时：用线上 `pg_dump --schema-only` + `_sqlx_migrations` 数据在本地恢复，启动新镜像验证升级路径。

### 上游自带、与 fork 无关的失败用例

在上游基线上同样失败，归类为继承失败，不算合并回归：

- WSL 下 `providers/openai` 的 `transport/websocket`、`transport/latency` 时序类用例十余个（依赖真实计时，机器负载高时波动）。
- `websocket_pong_timeout_diagnosis_…`（pong 超时诊断，同为计时波动）。

## 4. 发布

合回 `dev` 后按 [GIT_WORKFLOW.md](GIT_WORKFLOW.md#发版) 发版：fork 版本号前三段跟上游、第四段从 1 开始，
构建用 `CPR_BUILD_TYPE=fork`（见 [deploy.md](deploy.md)），按 `deploy/89.md` 蓝绿发布。
