# 分支、版本与发布流程

适用仓库：`wuxiran/codex-proxy-rs`（本 fork）。从 3.18.3.2 起按本文执行，与 sub2api-product-team 的流程一致。
上游的 [CONTRIBUTING.md](../../CONTRIBUTING.md) 写的 PR 目标分支是 `main`，那是上游仓库的约定；本 fork 的 PR 目标是 `dev`

## 一句话

日常开发进 `dev`，上线的代码只在 `main`；`main` 上每个提交都对应一次 89 发布

## 分支

| 分支 | 用途 | 谁能直接推 |
| --- | --- | --- |
| `main` | 已发布的代码。89 跑的镜像必须能在这里找到对应提交 | 只接受从 `release/*`、`hotfix/*` 合入，禁止强推 |
| `dev` | 下一个版本的开发主线。`release/fork-version.yaml` 写的是正在开发的版本号 | 功能分支合入，禁止强推 |
| `feat/<主题>`、`fix/<主题>` | 单个功能或修复，从 `dev` 拉出，做完合回 `dev` | 自己的分支随意 |
| `release/<版本>` | 准备发版时从 `dev` 拉出，只收修复，不收新功能 | 发版负责人 |
| `hotfix/<主题>` | 线上紧急修复，从 `main` 拉出 | 发版负责人 |
| `sync/upstream-v<上游版本>` | 合并上游 zyycn/codex-proxy-rs，从 `dev` 拉出，做完合回 `dev` | 合并负责人 |

本仓库的 `remote.origin.fetch` 只配了 `main`，新检出先执行一次
`git remote set-branches --add origin dev`，否则 `origin/dev` 拉不下来

## 版本号

- 格式是四段：`上游版本.fork 修订号`，例如 `3.18.3.2` 表示基于上游 `v3.18.3` 的第 2 个 fork 版本
- 上游版本不变时，每次发布第四段加 1。合并了新的上游版本后，前三段跟上游，第四段从 1 重新开始（`3.19.0.1`）
- fork 版本号只记在 `release/fork-version.yaml`。`dev` 上是下一个要发的版本，`main` 上是已发布的版本
- `release/version.yaml` 是上游文件，保持上游的三段版本，合并上游时直接取上游。它会编进二进制并参与插件的
  semver 兼容判断，四段版本号写进去会解析失败，所以 fork 修订号不写在这里
- 镜像标签：`codex-proxy-rs:<fork 版本>-<提交前 7 位>`，例如 `codex-proxy-rs:3.18.3.2-1a2b3c4`。
  从 `dev` 或功能分支构建的测试镜像写成 `<fork 版本>-dev-<提交>`，不允许上 89
- git tag：`fork-v<fork 版本>`，例如 `fork-v3.18.3.2`。`v3.x.y` 是上游的 tag，fork 不占用这个前缀

## 日常开发

```bash
git fetch origin
git worktree add -b feat/<主题> ~/worktrees/cpr-<主题> origin/dev   # 从 dev 拉分支
# ...开发、提交...
git push -u origin feat/<主题>
# 向 dev 提 PR，CI 通过后合并
```

- 一个分支只做一件事。先提交再构建：镜像只能从已提交的代码构建，工作区有未提交改动时不打镜像
- 合并前先把 `dev` 合进来或 rebase 到 `origin/dev`，解决冲突后再合
- worktree 固定在 WSL 侧建，用完在同一侧 `git worktree remove`；两侧都不要跑 `git worktree prune`

## 发版

1. 从 `dev` 拉 `release/<版本>`，此后这个分支只收修复
2. 在 release 分支上回归：按改动范围跑 clippy、测试、前端构建，验证涉及的页面和接口
3. 合并到 `main`（不能改写历史），从 `main` 的这个提交 `git archive` 出干净目录，构建镜像（`CPR_BUILD_TYPE=fork`，
   `CPR_GIT_SHA` 传该提交），并在同一目录生成迁移清单
4. 按 [deploy/89.md](../../deploy/89.md#发版) 用 `rollout.py` 发布。带迁移的版本按 [migrations.md](migrations.md#带迁移发版) 处理
5. 验收通过后，在该提交打 tag `fork-v<版本>` 并推送。tag 必须和 `release/fork-version.yaml` 一致
6. 把 `main` 合回 `dev`，并把 `dev` 的 `release/fork-version.yaml` 改成下一个版本号

发版前先 `rollout.py status` 看 89 现在跑的是什么镜像、部署目录里有没有别人刚放的包。两个会话同时发会互相覆盖

发布后检查：gate `/healthz` 返回 204，新槽位 healthy，sub2api 侧没有新增拒连，切换后几分钟内请求没有新增 5xx

## 紧急修复

1. 从 `main` 拉 `hotfix/<主题>`，只改必须改的
2. 测试、合并到 `main`、构建、发布，版本号第四段加 1
3. 把 `main` 合回 `dev`，保证 `dev` 也有这个修复

## 合并上游

流程见 [UPSTREAM_SYNC.md](UPSTREAM_SYNC.md)。同步分支从 `dev` 拉出，合并完成后合回 `dev`，随下一次发版上线

## 当前状态（2026-10-03）

- `main`：`3.18.3.2`，tag `fork-v3.18.3.2`，89 在跑这个版本
- `dev`：`3.18.3.3` 开发中
- `3.18.3.2` 的线上镜像是 `ticket-center-20261003-ef4067a7dc6b`，在本流程生效前从工作区构建；运行时源码与 tag 逐文件一致，
  只差本文等流程文件。下一版起镜像按上文的标签规则从 `main` 的提交构建
