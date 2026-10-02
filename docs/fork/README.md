# 本 fork 说明

本仓库是 [zyycn/codex-proxy-rs](https://github.com/zyycn/codex-proxy-rs) 的 fork。上游文档（`README.md`、`docs/*.md`、
`deploy/README.md`、`backend/migrations/README.md`）保持上游原样，fork 的增补全部写在本目录：

| 文档 | 内容 |
| --- | --- |
| [api.md](api.md) | fork 新增或改变的管理 API：账号票据与「已过期」、用量与费用来源、代理质量与批量、免登录导入、固定 state / 观澜复活 / 遍历代理、turn-state 模板 |
| [architecture.md](architecture.md) | fork 组件、计量子表、密文恢复、状态归属与 worker |
| [deploy.md](deploy.md) | fork 部署文件、构建类型、回退与升级注意 |
| [migrations.md](migrations.md) | 9xxx 迁移编号、CRLF 字节谱系、带迁移发版 |
| [UPSTREAM_SYNC.md](UPSTREAM_SYNC.md) | 合并上游的流程 |
| [FORK_HOOKS_REGISTRY.md](FORK_HOOKS_REGISTRY.md) | fork 留在上游文件里的改动登记与合并策略 |

## 管理端入口

- 「票据管理 → 账号票与预热」集中管理 OpenAI OAuth 账号的开关、打票和连接状态，详情中提供遍历代理、重新捕获和上游票据记录
  行为与效果边界见 [api.md](api.md#固定自身-state观澜复活与遍历代理)
- 账号列表的「成本/到期」列与「已过期」状态、代理质量检测与批量操作、测智台、请求日志、经营日报、
  turn-state 模板管理、系统设置里的免登录导入（按号商多配置）。

## 代码组织约定

fork 逻辑放在 fork 自有文件（`fork_<主题>.rs`、`*.fork.ts` / `*.fork.vue`、`docs/fork/`）；上游文件里只留一行调用或接线，
并带 `fork: <主题>` 标记，登记在 [FORK_HOOKS_REGISTRY.md](FORK_HOOKS_REGISTRY.md)。
用 `python3 tools/fork_divergence.py` 查看 fork 对上游文件的侵入面。
