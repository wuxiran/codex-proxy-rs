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

## 后端接线

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `gateway-admin/src/lib.rs` | B+A | fork 模块声明块（`mod fork;`、`ops_report`、`ticket_cipher`、`ticket_revive`、`turn_state_renewal` 及两行 `pub use`）；`AdminServices.fork`、`AdminRuntimePorts.fork` 字段及其解构、初始化；`fork::AccountsDeps::new(..)` 一个实参；`fork::attach(..)` 一行 | `gateway-admin/src/fork.rs`（fork 服务、运行目录、3 个 worker 注册、访问器 `ops_report()` / `public_import()` / `openai()`） | 取上游，补回 16 行。`attach` 要放在上游 worker 登记之后、`AdminBundle` 构造之前；它从 `services` 里取上游已建好的 proxies、account_groups、accounts、credentials |
| `gateway-admin/src/use_case/accounts.rs` 的 `new` | B | 构造函数末尾一个 `fork: crate::fork::AccountsDeps` 形参 | `fork.rs` 的 `AccountsDeps` | 上游改构造函数签名时，保留末尾这个形参 |
| `apps/gateway/src/bootstrap.rs` | B | `ForkRuntimePorts::under(host.runtime_data_dir())` 一行（要在 `host` 被 `initialize` 消费之前）；`fork: fork_ports` 字段；`gateway_api::initialize` 的 `turn_state` 实参 | `gateway-admin/src/fork.rs` | 取上游，补回 3 行 |
| `gateway-store/src/bundle.rs` | B | `AdminStorePorts::new(..)` 之后链一个 `.with_ops_report(..)` | `gateway-store/src/postgres/ops_report.rs` | 取上游，补回链式调用 |
| `gateway-api/src/lib.rs` | B | `mod public_import;`（fork 块）；`initialize` 的 `turn_state` 形参；`ApiState.turn_state` 字段及初始化；`.merge(public_import::router())`；`SessionState::turn_state` 实现 | `gateway-api/src/public_import.rs`、`admin/turn_state.rs` | 取上游，补回 12 行 |
| `gateway-api/src/auth.rs` | B | `SessionState::turn_state()` 默认方法（返回 `None`） | — | 取上游，补回声明读取与累积观测 |
| `gateway-api/src/admin/mod.rs` | B | fork 模块声明块（`fork_routes`、`ops_report`、`public_import`、`request_log`、`turn_state`）；`.merge(fork_routes::router())` 一行 | `admin/fork_routes.rs`（汇总全部 fork 管理路由） | 取上游，补回声明块和 1 行 merge |
| `gateway-api/src/admin/accounts/mod.rs` | B | fork 块：`mod fork_credentials;`、`mod fork_handlers;`、两行 `pub use`、`AccountListStatus` 导入 | `accounts/fork_handlers.rs`（票据、rotate、turn-state 打票与遍历、测智台）、`accounts/fork_credentials.rs`（rotate 请求体与校验） | 取上游，补回 fork 块 |
| `gateway-api/src/admin/accounts/handlers.rs`、`credentials.rs` | — | 无，与上游一致 | 同上 | 直接取上游 |

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
| `openai/tests/admin.rs` | D+C | worker 数断言为 11（上游 8 + fork 的复活、云端打票、WS warmer）；`oauth_transport_settings…` 只断言 `transport` 字段（fork 的 OAuth 文档多出字段）；7 处 `pub(crate)`、`mod mint_publication;` | `tests/turn_state_pin.rs` 的 `admin_rotation` 模块、`tests/admin/mint_publication.rs` | 取上游，worker 数改成「上游数 + 3」，补断言收窄和可见性 |
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
| `gateway-api/tests/architecture.rs` | D | 源码清单、测试清单之后各一个 `expected.extend([..])` 块，登记 fork 自有文件（清单运行时排序，上游列表保持原样） | — | 取上游，补回两个 `extend` 块；新增 fork 文件时在块里登记 |
| `gateway-api/tests/admin/mod.rs`、`admin/accounts/mod.rs` | D | fork `mod` 行；夹具字段 | — | 取上游，补回 |

## Provider 执行路径

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `providers/openai/src/provider/mod.rs` | B | `mod fork_served;` | `provider/fork_served.rs` | 取上游，补 1 行 |
| `providers/openai/src/provider/execution.rs` | A | 9 行带 `fork: served-mismatch` 标记的调用：建 `served_watch`、`routed`、两处 `observe` + `annotate`、建好 `observation_state` 后的一处 `annotate`、两处 `completed`、两处 `log_patch` 包住请求日志补丁 | `provider/fork_served.rs`、`route_pair.rs`；注票时传递当前 route 指纹、初始化 HTTP 全部模型头判定 | 取上游后补回。`observe` 要在本块的 metadata 合并之后、`attach_openai_session_update` 之前；`completed` 与 `pin.completed` 同条件 |
| `providers/openai/src/provider/observation.rs` | B | `fork_metadata` 字段及其初始化；`provider_metadata` 里一行 `metadata.extend(..)` | `provider/fork_served.rs` 的 `annotate` | 取上游，补 3 处 |
| `providers/openai/src/transport/canonical.rs` | B | `declared_models()`、初始 HTTP 头及逐事件累积的 mismatch 事实 | — | 取上游，补回声明读取与累积观测 |
| `providers/openai/src/credential/selector.rs` | A | `drop_route_pair` 方法；`capture_response_cookies` 里 `captured_expiry(..)` 一处 | `route_pair.rs` | 取上游，补回 2 处 |
| `providers/openai/src/transport/websocket/pool/state.rs`、`handshake.rs` | B | `CodexWebSocketConnectionMetadata.route_pair` 字段及其 `None` 初始化 | `route_pair.rs` | 取上游，补字段 |
| `providers/openai/src/transport/response_meta.rs` | B | `has_model_mismatch`、`event_has_model_mismatch`：所有模型头及多值分别对照 | canonical decoder、WS reducer/stream | 取上游，补回累积检查，不能只用展示用的 effective_model |
| `providers/openai/src/transport/protocol/responses.rs`、`websocket/model.rs`、`websocket/handshake.rs`、`client_sse.rs` | B | 本地 `minted_turn_state_route` 字段及传递；云端票新链按路由隔离池 profile | `turn_state_pin.rs`、`websocket/coordinator.rs` | 取上游，补回控制字段与池键，不写入上游正文 |
| `providers/openai/src/transport/websocket/coordinator.rs` | A | 握手后给 `metadata.route_pair` 赋值；发送正文前核对云端票路由，不符则 discard | `route_pair.rs` 的 `RoutePairRef::handshake` | 取上游，补回 |
| `providers/openai/src/transport/websocket/exchange/mod.rs` | B | `CodexWebSocketResponseMetadataUpdate` 的 `route_pair`、`discard_connection`、`served_mismatch` 字段 | — | 取上游，补字段 |
| `providers/openai/src/transport/websocket/exchange/stream.rs` | A+B | 上述两个字段的初始化；`ServedModelMismatch` 丢弃原因；终态归还前的模型对照与丢弃分支，请求侧显式传入是否允许丢连接，模拟运行关闭 | — | 取上游，补回 4 处。守卫分支要排在正常的 `Completed \| Interrupted` 分支之前 |
| `providers/openai/tests/provider/contract/mod.rs`、`tests/credential/contract/mod.rs` | D | `mod fork_served_mismatch;`、`mod fork_route_pair;` | 同名测试文件 | 取上游，补声明 |

<!-- 后端接线、数据结构、热点文件的登记随各阶段补充 -->
