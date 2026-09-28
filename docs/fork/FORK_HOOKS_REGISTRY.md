# Fork 钩子登记表

上游基线：zyycn/codex-proxy-rs `3915aa25`（v3.17.0）。度量：`python3 tools/fork_divergence.py`。

约定：fork 逻辑放 fork 自有文件（Rust `fork_<主题>.rs`，前端 `*.fork.ts` / `*.fork.vue`，文档 `docs/fork/`）；
上游文件只留一行调用或接线，并带 `fork: <主题>` 标记。

类型：

- **A** 一行钩子：上游文件里调用 fork 函数的一行。
- **B** 接线：模块声明、导出、字段、路由或菜单的接入行。
- **C** 行为修改（残留）：直接改了上游逻辑，合并时人工比对。
- **D** 测试：fork 测试模块声明、共用辅助函数的可见性放宽、编译必需的夹具适配。

合上游时：A / B / D 类文件取上游版本，再补回表里的标记行；C 类按「合并策略」处理。

## 文档

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `README.md` | B | 末尾一行指向 `docs/fork` | `docs/fork/README.md` | 取上游，末尾补回 |
| `docs/api.md`、`docs/architecture.md`、`deploy/README.md`、`backend/migrations/README.md` | — | 无，与上游一致 | `docs/fork/{api,architecture,deploy,migrations}.md` | 直接取上游 |

## 前端接线

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `frontend/src/router/routes.ts` | B | import 一行；`...forkPublicRoutes`、`...forkAdminRoutes` 各一行 | `router/routes.fork.ts` | 取上游，补回 3 行 |
| `frontend/src/layout/components/AppSidebar.vue` | A | import 一行；`insertForkNavItems(navItems)` 一行（在 `navItems` 定义之后） | `layout/components/menu.fork.ts`（菜单项与锚点） | 取上游，补回 2 行；上游改了 `/accounts`、`/usage` 的路径时同步 `menu.fork.ts` 的锚点 |
| `frontend/src/api/index.ts` | B | 文件开头 `export * from './index.fork'`（lint 要求全文件按字母序） | `api/index.fork.ts` | 取上游，开头补回 |

## 测试

测试模块统一命名 `fork_<主题>.rs`，和被测的上游测试文件同目录；父模块里用带 `// fork:` 标记的 `mod` 行声明。
根目录级的测试模块没有对应的生产模块，会违反架构约束，所以都放在镜像生产目录的子目录下。

### gateway-core / gateway-host

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `gateway-core/tests/engine/coordinator.rs` | D+B | 测试夹具的 `pub(super)`；`billing` 字段及其赋值（`// fork: billing`）；`Script::AttributedStream` 变体及其分支（`// fork: attribution`） | `tests/engine/fork_coordinator.rs`（归因与计费用例） | 取上游，补可见性、`billing` 字段和 `AttributedStream` 变体 |
| `gateway-core/tests/engine/provider.rs` | — | 无，与上游一致 | `tests/engine/fork_provider.rs` | 直接取上游 |
| `gateway-core/tests/engine/mod.rs` | D | `mod fork_coordinator;`、`mod fork_provider;` | — | 取上游，补 2 行 |
| `gateway-host/tests/proxy_probe.rs` | — | 无，与上游一致 | `tests/outbound/fork_proxy_probe.rs` | 直接取上游 |
| `gateway-host/tests/outbound/mod.rs` | D | `mod fork_proxy_probe;` | — | 取上游，补 1 行 |

### gateway-admin

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `tests/use_case/proxies.rs` | D | 夹具字段适配（`auto_location`、`exit_geo`、`quality` 等）、`fork: TestProxiesFork` 字段 | `tests/use_case/fork_proxies.rs` | 取上游，补夹具字段 |
| `tests/use_case/accounts.rs` | D+C | 替身里 `impl ProviderAdmin` 的 4 个 turn-state 方法、`mutate_account()`、`commit_import` 记录 `reject_existing`、辅助函数 `pub(super)` | `tests/use_case/fork_accounts.rs` | 取上游后补回；第三阶段引入 `ForkProviderAdmin` 后 turn-state 方法可搬走 |
| `tests/model/quota_forecast.rs`、`quota_forecast_sampling.rs` | D | 辅助函数 `pub(super)` | `tests/model/fork_quota_forecast*.rs` | 取上游，补可见性 |
| `tests/use_case/credentials/openai.rs` | D | `service` 的 `pub(super)`、模块路径适配 | `tests/use_case/credentials/fork_openai.rs` | 取上游，补可见性 |
| `tests/use_case/mod.rs`、`tests/model/mod.rs`、`tests/use_case/credentials/mod.rs` | D | fork 模块声明块；运行目录夹具字段 | — | 取上游，补声明块 |

### providers/openai、providers/xai

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `openai/tests/provider/contract/mod.rs` | D | fork `mod` 声明组；上游用例里 2 处带标记的 `outbound_proxy_endpoint` 断言 | `contract/fork_encrypted_content.rs`、`fork_diagnostic_egress.rs`、`fork_turn_state_pin.rs` | 取上游，补声明组和 2 处断言 |
| `openai/tests/admin.rs` | D+C | worker 数断言为 11（上游 8 + fork 的复活、云端打票、WS warmer）；`oauth_transport_settings…` 只断言 `transport` 字段（fork 的 OAuth 文档多出字段）；7 处 `pub(crate)` | `tests/turn_state_pin.rs` 的 `admin_rotation` 模块 | 取上游，worker 数改成「上游数 + 3」，补断言收窄和可见性 |
| `openai/tests/transport/websocket_pool.rs`、`transport/canonical.rs`、`credential/admin.rs`、`credential/cookie.rs`、`provider/mod.rs` | — | 无，与上游一致 | `transport/fork_ws_warm_pool.rs`、`transport/fork_billing_identity.rs`、`credential/cdk.rs`、`credential/fork_cookie.rs` | 直接取上游 |
| `openai/tests/credential/mod.rs`、`transport/mod.rs` | D | fork `mod` 声明组 | — | 取上游，补声明组 |
| `openai/tests/support.rs`、`credential/types.rs`、`config.rs`、`main.rs` | D | `set_turn_state_pin`（要访问 store 私有字段）、1 个夹具字段、4 处复活配置断言、`mod turn_state_pin;` | 原文件 | 取上游后补回 |
| `xai/tests/provider/contract.rs` | D | 8 处 `pub(super)`；fork 的 `proxy` 夹具字段 | `xai/tests/provider/fork_attribution.rs` | 取上游，补可见性和夹具字段 |
| `xai/tests/provider/mod.rs` | D | `mod fork_attribution;` | — | 取上游，补 1 行 |
| `xai/tests/transport/canonical.rs` | C | 按 fork 的双费用口径改了上游断言（已带注释标记） | 原文件 | 取上游后按双费用口径重新调整断言 |

### gateway-store、gateway-api

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `gateway-store/tests/postgres/provider_accounts/mod.rs` | D | 3 行 `mod fork_*`；3 处状态筛选的 `.into()`；`exit_geo: None` | `provider_accounts/fork_billing.rs`、`fork_expired_status.rs`、`fork_turn_state.rs` | 取上游，补声明和适配 |
| `gateway-store/tests/postgres/proxies.rs` | D | `exit_geo: None`；`context()`、`success()` 的 `pub(super)` | `postgres/fork_proxy_quality.rs` | 取上游，补字段和可见性 |
| `gateway-store/tests/postgres/execution.rs` | D | 2 处 `billing: Default::default()`；2 个辅助函数 `pub(super)` | `postgres/fork_execution_billing.rs` | 取上游，补字段和可见性 |
| `gateway-store/tests/postgres/mod.rs` | D | fork `mod` 行；迁移表清单里的 fork 表 | — | 取上游，补回 |
| `gateway-api/tests/admin/accounts/mod.rs` | D | 2 行 `mod`；2 处 `.into()`；`AccountUsageView` 夹具字段 | `accounts/fork_turn_state_wire.rs` | 取上游，补声明和适配 |
| `gateway-api/tests/admin/accounts/presenter.rs` | D | 4 个预测字段（`None` / 空） | `accounts/fork_presenter.rs` | 取上游，补字段 |
| `gateway-api/tests/admin/proxies.rs` | D+C | 共用替身 `MemoryProxies` / `SuccessfulProbe` 里的质量报告存储、重复地址检查、`exit_geo`（44 行，必须和上游替身写在同一个 impl 里）；`request()` 的 `pub(super)` | `admin/fork_proxy_quality.rs` | 取上游替身，把 fork 的质量与重复检查补回同一个 impl |
| `gateway-api/tests/admin/accounts/handlers.rs` | C | 上游的「rotate 路由不存在」用例改成 `fork_rotation_route_stays_exposed_and_validates_its_material` | 原文件 | 取上游后把该用例改回 fork 版本（fork 保留 `/accounts/rotate`） |
| `gateway-api/tests/architecture.rs` | D | 测试文件冻结清单里的 fork 文件（带 `// fork:` 标记） | — | 取上游，补回 fork 文件行 |
| `gateway-api/tests/admin/mod.rs`、`admin/accounts/mod.rs` | D | fork `mod` 行；夹具字段 | — | 取上游，补回 |

<!-- 后端接线、数据结构、热点文件的登记随各阶段补充 -->
