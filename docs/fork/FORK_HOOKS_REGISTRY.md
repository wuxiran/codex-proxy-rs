# Fork 钩子登记表

上游基线：zyycn/codex-proxy-rs `ab8c61b1`（`v3.29.1` 之后 4 个提交，含 Rust 1.99.1 / Node 26.12.0 工具链升级）。
度量：`python3 tools/fork_divergence.py --base ab8c61b1`。

约定：fork 逻辑放 fork 自有文件（Rust `fork_<主题>.rs`，前端 `*.fork.ts` / `*.fork.vue`，文档 `docs/fork/`）；
上游文件只留一行调用或接线，并带 `fork: <主题>` 标记。fork 测试一律放 `tests/` 下，`src` 不放 `#[cfg(test)]`。
用 `grep -rn "fork:" backend frontend/src` 可列出全部标记行，与本表逐项对照。

类型：

- **A** 一行钩子：上游文件里调用 fork 函数的一行。
- **B** 接线：模块声明、导出、字段、路由或菜单的接入行。
- **C** 行为修改（残留）：直接改了上游逻辑，合并时人工比对。
- **D** 测试：fork 测试模块声明、共用辅助函数的可见性放宽、编译必需的夹具适配。

合上游时：A / B / D 类文件取上游版本，再补回表里的标记行；C 类按「合并策略」处理。
上游移动或重命名了文件时，fork 文件跟着搬到新位置、旧路径删掉（本表路径即当前位置）。

## 上游已有等价能力、fork 不再维护的改动

下列能力以上游实现为准，合并时若在 fork 侧看到残留一律删除，不要补回：

| 原 fork 改动 | 现用上游 |
| --- | --- |
| Provider 预输出阶段换号重试 | Core `transient_retry_policy`；500/502/504 无 `server_error` 正文不重试 |
| `/api/admin/proxies/batch-create`、`ProxyBatchCreateModal`、`useProxyBatchRunner`、fork 批量测试 | `/api/admin/proxies/create` / `test` 的数组请求与上游批量运行器；fork 只保留批量删除 |
| 账号状态列「并发 x/y」文字 | 上游用量列容量标记与并发详情（后端 `concurrency` 字段仍下发） |
| `RequestQueueCard` | 上游 `ConcurrencyPolicyCard` |
| 删账号前批量解引用历史表 | 上游 `0028` 已取消外键 |
| 分组成本 SWR 缓存 | 上游 analytics 投影（账号列表用量投影缓存仍是 fork 的，见下） |
| 导入数量上限与混合文档解析 | 上游分批提交与逐账号拆分；fork 只保留 Sub2API 导出识别 |
| `selector.update_session_affinity` 会话续期 | 上游 `admit_session` |
| 额度预测在回落同窗口时重置周期基线 | 上游 #414 的跨回落累计；fork 只保留「回落后清空曲线」 |
| `AccountProxyField` 未测通过不可选 | 上游「已禁用不可选」；fork 只在描述里附出口地区与质量等级 |

## 仓库根与文档

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `README.md` | B | 末尾 `<!-- fork -->` 一段指向 `docs/fork` | `docs/fork/README.md` | 取上游，末尾补回 |
| `AGENTS.md` | B | 末尾两条：分支流程见 `GIT_WORKFLOW.md`；89 发版只走 `rollout.py` | — | 取上游，补回 2 行 |
| `.gitattributes` | B | `backend/migrations/*.sql -text`、`.frozen-sha256 text eol=lf` | [migrations.md](migrations.md) | 保留，不要去掉 |
| `.gitignore` | B | `__pycache__/`、`*.py[cod]`（`tools/revive`、`deploy/rollout.py`） | — | 取上游，补回 |
| `.github/workflows/ci.yml` | B | `push` / `pull_request` 分支列表各加一行 `dev` | — | 取上游，补回 2 行 |
| `REQUEST_ATTRIBUTION.md` | — | fork 自有：请求归因（账号 / 出口 / 模型）事实说明 | 本文件 | 保留 |
| `deploy/Dockerfile` | B | `ARG CARGO_BUILD_JOBS=2` 并传给 `cargo build --jobs` | [deploy.md](deploy.md) | 取上游，补回 2 行 |
| `frontend/vite.config.ts` | B | dev server 代理 `/guanlan` → 观澜兑换站 | `api/modules/guanlanCdk.ts` | 取上游，补回 1 段 |
| `modules/ui`、`modules/plugins` | — | 子模块指针与上游不同 | — | 合并时核对指针：前端按 `@codex-proxy/ui` v0.5.0（`Z*` 组件）编译 |
| `docs/api.md`、`docs/architecture.md`、`deploy/README.md`、`backend/migrations/README.md`、`release/notes.md`、`release/version.yaml` | — | 无，与上游一致 | `docs/fork/*.md`、`release/fork-version.yaml` | 直接取上游 |

## 前端接线

上游 UI 库升到 v0.5.0（`Base*` → `Z*`），目录改为 kebab-case；fork 自有组件引用 `@codex-proxy/ui` 的 `Z*` 组件，
图表仍用宿主 `components/charts` 的 `BaseChart`。fork 自有页面（`views/{logs,ops-report,public-import,public-import-admin,testbench,turn-state}/`、
`stores/modules/testbench.ts`、`composables/useAccountConfigurations.ts`、`utils/format.fork.ts`、`api/modules/{guanlanCdk,public-import,turn-state}.ts`）
没有钩子，不在表内。

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `router/routes.ts` | B | import 一行；`...forkPublicRoutes`、`...forkAdminRoutes` 各一行 | `router/routes.fork.ts` | 取上游，补回 3 行 |
| `router/index.ts` | A | 守卫里 1 处：`/import/:token` 密链导入页既不恢复会话也不跳登录 | — | 取上游，补回 |
| `layout/components/AppSidebar.vue` | A | import 一行；`insertForkNavItems(navItems)` 一行（`navItems` 定义之后） | `layout/components/menu.fork.ts`（锚点 `/accounts`、`/usage`，回退 `/plugins`） | 取上游，补回 2 行；上游改菜单路径时同步锚点 |
| `api/index.ts` | B | 开头 `export * from './index.fork'` | `api/index.fork.ts` | 取上游，开头补回 |
| `api/modules/accounts.ts` | B | `import { API_BASE_URL }`（turn-state SSE）；状态联合类型加 `expired`；`AccountBilling`、`AccountTicket`、turn-state / 打票 / 测智台请求与返回类型及函数 | 后端 `fork_handlers.rs` | 取上游，补回类型与函数 |
| `api/modules/proxies.ts` | B | `exitGeo` / `quality` 字段与类型；`quality-check` / `quality-report` / `batch-delete` 函数 | 后端 `admin/proxies.rs` fork 段 | 取上游，补回 3 段 |
| `api/modules/usage.ts` | B | 用量记录查询加 `accountId?` 筛选 | `AccountRecentErrors.vue`、turn-state 历史 | 取上游，补回 1 字段 |
| `api/modules/settings/index.ts`、`views/settings/composables/useSettingsForm.ts`、`views/settings/index.vue` | B | `requestLogEnabled` / `requestLogTestKeyId` 两字段的类型、表单默认值与提交；`RequestLogCard` import 与挂载 | `views/settings/components/RequestLogCard.vue` | 取上游，补回 |
| `views/accounts/index.vue` | B | 票据列与弹窗、近 24h 报错弹层、行内 `SyncedSwitch` 调度开关、自动刷新、窄屏操作列、观澜复活的 import 与模板接线（约 15 处带标记） | `components/AccountTicket{Cell,Modal}.vue`、`AccountRecentErrors.vue`、`AccountAutoRefresh.vue`、`components/SyncedSwitch.vue`、`composables/useAccountConfigurations.ts` | 取上游，逐处补回 |
| `views/accounts/constants.ts` | B | 列定义：`enabled`（调度）、`ticket`（成本/到期）、状态列 `lg`、操作列 `2xl`；`expired` 标签与色值；`errorRateDisplay` | — | 取上游，补回 |
| `views/accounts/components/AccountTableActions.vue` | B | `revive` / `ticket` 事件与两个菜单项 | 同上 | 取上游，补回 |
| `views/accounts/components/AccountOverviewCards.vue` | A | 「账号池规模」说明附已过期计数 | 后端 `summary.expired` | 取上游，补回 1 行 |
| `views/accounts/components/account-status-badge/presenter.ts` | A | `expired` 不显示刷新退避 | — | 取上游，补回 1 条件 |
| `views/accounts/components/AccountProxyField.vue` | A | 选项描述附出口地区与质量等级；可选规则沿用上游 | — | 取上游，补回描述拼接 |
| `views/accounts/components/account-quota-panel/index.vue` | B | `import AccountQuotaBurnChart from './BurnChart.vue'`；曲线插槽 | `account-quota-panel/BurnChart.vue`、`burnPresenter.ts` | 取上游，补回 2 处；保留上游预测入口，避免重复绑定账号更新事件 |
| `views/accounts/components/account-quota-summary-cell/index.vue`、`Entry.vue`；`components/account/account-usage-window/presenter.ts` | C | 模型计价与真实上游费用并列、本机 USD 已用与近似额度 | 账号 `usage.billing` 投影 | 延续上游容量对齐与 Token 摘要，保留两种费用来源与 `usd` 字段 |
| `views/accounts/components/AccountUsagePanel.vue` | C | 总计费取模型计价；逐模型按请求 / 路由 / 计费模型拆行并列双费用与差额 | `usage.models[].billing` | 取上游布局，补回 billing 列 |
| `views/accounts/components/account-create-modal/{index.vue,presenter.ts,AccountImportFields.vue}`、`utils/accountCreate.ts` | B+C | `cdk` 导入模式（观澜 CDK）、Sub2API 导出文案、拖放多文件合并提示 | `api/modules/guanlanCdk.ts`、`utils/importDocuments.ts` | 取上游，补回 cdk 分支与文案 |
| `views/accounts/composables/useAccountOnboarding.ts` | B | 用 `importDocuments.ts` 识别 Sub2API 导出 | `utils/importDocuments.ts` | 取上游，补回 import 与调用 |
| `views/accounts/composables/useAccountMutations.ts` | B | 观澜复活动作；行内调度开关的局部更新（只提交调度字段，成功后按详情替换本行） | — | 取上游，补回 |
| `views/accounts/composables/useAccountsQuery.ts` | B | 自动刷新间隔（浏览器记忆，非法值回退默认） | `AccountAutoRefresh.vue` | 取上游，补回 |
| `views/accounts/composables/useAccountEditor.ts` | C | 读取 `credentialConfiguration` 的方式 | — | 以上游为主，比对后保留 fork 需要的字段 |
| `views/proxies/index.vue` | B | 质量检测列 / 批量质量检测 / 报告弹窗、批量删除、端点复制的 import 与模板接线（约 15 处带标记） | `components/ProxyQualityCell.fork.vue`、`ProxyEndpointCopy.fork.vue`、`ProxyQualityReportModal.vue`、`composables/useProxyQuality.fork.ts`、`useProxyBatchDelete.fork.ts`、`presenter.fork.ts` | 取上游，逐处补回；不要补回批量添加 |

前端 fork 页面对上游接口的依赖（上游改这些接口时要跟着改）：

- `views/turn-state/components/AccountTurnStateHistory.vue`：上游票据记录来自 `GET /api/admin/usage/records?accountId=`、
  `/usage/records/detail`，trace 经 `api/modules/turn-state.ts` 的 `getRequestTrace` 从 `downloadDiagnostics`
  （`/api/admin/diagnostics/export?requestId=`）导出包里取；`utils/turnStateHistory.ts` 按 SHA-256 字段名读 state 头长度。
- `views/accounts/components/AccountRecentErrors.vue`：报错明细来自 `GET /api/admin/usage/records?status=error&accountId=`；
  列表里的计数来自 fork 字段 `recentErrors`。
- `views/testbench/`：`POST /api/admin/accounts/test-bench` SSE（fork）；账号与分组列表用上游接口。

## 后端接线

### apps/gateway

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `src/bootstrap/startup.rs` | B | `ForkRuntimePorts::under(host.runtime_data_dir())`（必须在 `host` 被 `initialize` 消费之前）；启动产物的 `fork_ports` 字段 | `gateway-admin/src/fork.rs` | 取上游，补回 3 行 |
| `src/bootstrap/server.rs` | B | 透传 `fork_ports`；`AdminRuntimePorts { fork: fork_ports }`；`gateway_api::initialize` 的 `Some(providers.openai.turn_state_service())` 实参 | 同上 | 取上游，补回 3 行 |
| `src/bootstrap/command.rs` | B | 透传 `fork_ports`；`ForkAccountsDeps::new(&store.admin_ports(), &fork_ports)` | 同上 | 取上游，补回 2 行 |
| `tests/architecture.rs` | D | crate 清单与依赖白名单登记 `turn-state`（4 行） | `crates/turn-state` | 取上游，补回 4 行 |

### gateway-core

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `src/settings/values.rs`、`routing/mod.rs`、`routing/snapshot.rs` | B | `request_log_enabled` / `request_log_test_key_id`：设置字段、默认值、构造方法、快照访问器与 RoutingPlan 投影 | 统一的 `SettingsValues` owner | 保留上游设置编译路径再补字段；不得恢复独立的旧设置结构 |
| `src/lib.rs` | B | `pub mod request_log;` | `src/request_log.rs`（进程内 300 条环形缓冲，`/api/admin/logs/recent` 的数据源） | 取上游，补回 1 行 |
| `src/account/store.rs` | B | `account_ticket_expires_at` 默认方法（返回 `None`） | `providers/openai/src/fork_account_ticket.rs` | 取上游，补回；夹具靠默认实现 |
| `src/account/location.rs` | B | `RequestLocation::from_geo(country_code, region, city, timezone)` | `gateway-host/src/proxy_probe.rs` 出口地理回填 | 取上游，补回 1 方法 |
| `src/engine/mod.rs` | B | `DiagnosticEgress { proxy, location }` 类型 | 遍历代理 / 测智台 | 取上游，补回 |
| `src/engine/provider.rs` | B | `ProviderResponseMetadata.outbound_proxy_endpoint` 字段、访问器与 `with_outbound_proxy`（元数据边界去掉代理凭据） | 归因 | 取上游，补回 |
| `src/engine/probe.rs` | B | `AccountProbeMode { Diagnostic, Business }`、`AccountProbeExecution`；连接测试返回 Provider 原始失败分类与执行事实 | 测智台、遍历代理 | 取上游，补回 |
| `src/engine/coordinator.rs` | C | `AccountSelection::Diagnostic { account, egress }`；每次尝试记录 `request.attempt_attribution`（路由模型 / 账号 / 出口）并用于最终归因；终结时带 `billing`；`with_request_log_capture(plan.request_log_enabled(), ...)` | 归因、计费、请求日志 | 取上游后逐处比对；不得恢复 fork 的预输出换号重试 |
| `src/engine/execution/service.rs` | C | 探测按 `mode` 选 `plan_diagnostic` / `plan`；`Business` 不允许覆盖出口；带 `egress` 的探测不计熔断、不写账号探测失败事实 | 测智台 | 取上游后补回分支 |
| `src/engine/observation.rs` | C | `billing: ModelBillingObservation` 的累积（`CalculatedCost` 总额、`observed_model`、`billing_model`）与重试清空 | 计量 | 取上游后补回 |
| `src/event.rs` | B | `ResponseMeta.observed_model` / `billing_model` 与 `with_observed_model` | xai / openai canonical | 取上游，补回 |
| `src/metering/mod.rs` | B | `ModelBillingObservation { response_model, billing_model, calculated_cost }` | 子表 `model_request_billing` | 取上游，补回 |

### gateway-admin

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `src/lib.rs` | B+A | fork 模块块（`mod fork;`、`ops_report`、`ticket_cipher`、`ticket_revive`、`turn_state_renewal`、两行 `pub use`）；`AdminServices.fork`、`AdminRuntimePorts.fork` 字段及初始化；`fork::AccountsDeps::new(&store, &fork_ports)` 实参；`fork::attach(..)` 一行；`#[expect(too_many_arguments)]` 的 fork reason | `src/fork.rs`（fork 服务、运行目录、3 个 worker 注册、访问器） | 取上游，补回约 16 行。`attach` 放在上游 worker 登记之后、`AdminBundle` 构造之前 |
| `Cargo.toml` | B | `ring`（票据密文） | `ticket_cipher.rs` | 取上游，补回 1 行 |
| `src/model/mod.rs`、`src/ports/mod.rs`、`src/use_case/mod.rs` | B | `account_tickets` / `ops_report` / `public_import` 模型；`ops_report` 端口；`account_tickets`、`public_import`、`turn_state_hunt`、`usage_projection_cache` 用例；`use_case::openai::OpenAiService` 窄 trait（由 `CredentialsService` 冻结 openai 实现后委托） | 各同名文件 | 取上游，补回声明块；`OpenAiService` 是刻意保留的分歧 |
| `src/use_case/accounts/mod.rs` 的 `new` | B | 构造函数末尾 `fork: crate::fork::AccountsDeps` 形参及 `expect` reason | `fork.rs` | 上游改签名时保留末尾形参 |
| `src/use_case/credentials/flow.rs`、`plugin_accounts.rs` | B | `import_new_accounts`（只新建 OAuth 账号，已存在整批拒绝）；`reject_existing: false` | 免登录导入 | 取上游，补回 |
| `src/use_case/proxies.rs` | C | `quality_check` / `batch_delete`；测试槽位占满时排队 20 秒再 429 | `gateway-host/src/proxy_probe.rs` | 取上游后补回两段 |
| `src/model/settings.rs` | B | `RuntimeSettingsValues` → 替换命令携带日志字段 | `ReplaceRuntimeSettings::from` | 取上游，补 2 字段 |
| `src/model/audit.rs` | B | `OutboundProxyQualityCheck` 审计分类 | — | 保留 |
| `src/model/accounts.rs` | B | `AccountListStatus { Status, TicketExpired }`、`AccountModelIdentity`、`AccountBillingAmounts` | 已过期目录状态、双费用 | 取上游，补回类型 |
| `src/model/proxies.rs` | B | `ProxyExitGeo`（含仅内存的 `timezone`）、`ProxyTestResult.exit_geo`、质量报告 / 结论类型 | — | 取上游，补回 |
| `src/model/provider_credentials.rs` | B | `reject_existing`；`AccountConcurrency`、`AccountRecentErrors`、票据事实 | 账号目录 | 取上游，补回 |
| `src/model/observability.rs` | B | `AccountModelUsage.identity` / `.billing`（上游把该类型搬到 observability 并改了 token 字段名） | `gateway-store/.../queries/fork_billing.rs` | 取上游，补 2 字段 |
| `src/model/quota_forecast.rs`、`quota_forecast_sampling.rs` | C | `window_start_at` / `curve` / `burn_percent_per_hour` / `exhaustion`；`project_exhaustion` 一处调用；回落时只清空 `curve` | — | 取上游估算口径（跨回落累计），补回曲线与耗尽外推 |
| `src/ports/store.rs`、`ports/provider.rs`、`ports/proxy.rs` | B | 票据 / 复活 / 近 24h 报错的默认方法；`TurnStateHunt*`、`TurnStateMint*`、`TurnStateRenewal` 类型与默认不支持的 Provider 方法；`record_quality` / `quality_report` 默认不可用、`ProxyProbe::quality` 默认；`ProxyProbe::test(Option<&OutboundProxy>, bool)` 是上游签名 | fork 用例 | 取上游，补回默认方法 |
| `gateway-plugin/sdk/src/call/services/settings.rs` | B | 生成的设置合同含日志字段 | `tests/use_case/service_contract.rs` | 用 `CPR_UPDATE_SERVICE_CONTRACT=1` 重新生成，不手改 |

### gateway-store

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `src/bundle.rs`、`src/config.rs`、`src/postgres/mod.rs` | B | `.with_ops_report(PgOpsReportSource::new(..))`；`ops_report.sub2api_database_url` 配置；`mod ops_report` | `src/postgres/ops_report.rs` | 取上游，补回 |
| `src/postgres/runtime_settings.rs`、`snapshot.rs` | C | 日志两列的 select / insert / update（`$37`、`$38`）、行结构与 `with_request_log` | 同文件 | SQL 占位符与 bind 顺序整体核对，保留上游管理员 Key 独立更新路径 |
| `src/postgres/execution/mod.rs` | C | `billing_upsert` CTE 写子表 `model_request_billing`（`$32`–`$35`） | 迁移 `9001` | 取上游 CTE 链后补回；占位符随上游编号顺延 |
| `src/postgres/proxies.rs` | C | `LEFT JOIN outbound_proxy_quality`、`exit_geo_from_row` / `quality_snapshot_from_row`、`record_quality` / `quality_report` 实现 | 迁移 `9002` | 取上游投影后补回 JOIN 与两函数 |
| `src/postgres/provider_accounts/admin_queries.rs` | C | 目录状态 CTE：`ticket_expired` 且非正常 / 限流 → `expired`；`summary_expired`；默认筛选排除 `expired` | 迁移 `9003` | 取上游后补回 CTE 分支 |
| `src/postgres/provider_accounts/{admin_adapter,mapping,core_adapter}.rs` | A/B | `.map(with_model_identity)`；`billing: usage.billing`；`account_ticket_expires_at` 实现 | — | 取上游，补回 |
| `src/postgres/observability/{model,mapping}.rs`、`queries/mod.rs`、`queries/accounts.rs`、`analytics/accounts.rs` | A/B | `identity` / `billing` 字段与 `split_model_identity`；`mod fork_billing;`；4 + 4 处 `fork_billing.take_*` / `plain_model_identity` 调用 | `queries/fork_billing.rs`（子表补算，只在管理端账号面板触发） | 取上游，补回 |

### gateway-api

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `Cargo.toml` | B | `turn-state` 依赖 | `crates/turn-state` | 取上游，补回 |
| `src/lib.rs` | B | `mod public_import;`；`initialize` 的 `turn_state` 形参与 `#[expect(too_many_arguments)]`；`ApiState.turn_state`；`.merge(public_import::router())`；`SessionState::turn_state` 实现 | `src/public_import.rs`、`admin/turn_state.rs` | 取上游，补回约 12 行 |
| `src/auth.rs` | B | `SessionState::turn_state()` 默认方法 | — | 取上游，补回 |
| `src/admin/mod.rs` | B | fork 模块块（`fork_routes`、`ops_report`、`public_import`、`request_log`、`turn_state`）；`.merge(fork_routes::router())` | `admin/fork_routes.rs` | 取上游，补回 |
| `src/admin/auth.rs` | B | `admin_request_id` 放宽为 `pub(crate)` | `public_import.rs` | 取上游，补回 |
| `src/admin/accounts/mod.rs` | B | `mod fork_credentials; mod fork_handlers;`、两行 `pub use`、`AccountListStatus` 导入 | `accounts/fork_handlers.rs`（票据、rotate、打票与遍历、测智台）、`fork_credentials.rs` | 取上游，补回 |
| `src/admin/accounts/wire.rs` | B | 状态筛选用 `AccountListStatus::parse`；`summary.expired`；`AccountTicketView`、`AccountBillingView`、云端打票请求体；`concurrency` / `recent_errors` / `ticket` 字段 | — | 取上游，补回 |
| `src/admin/accounts/presenter.rs` | C | 已过期判定、`estimated_quota_usd_display`、billing / served-mismatch / ticket 投影（4 段带标记） | — | 取上游后补回 |
| `src/admin/proxies.rs` | B+C | `quality-check` / `quality-report` / `batch-delete` 路由与请求体；`exit_geo` / `quality` 视图字段 | `gateway-admin::use_case::proxies` | 取上游批量框架，补回三条路由与字段；不补 `batch-create` |
| `src/admin/settings.rs` | B | `request_log_enabled` / `request_log_test_key_id`（`serde(default)`）4 处 | — | 取上游，补回 |

### gateway-host

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `src/proxy_probe.rs` | C | 出口地理叠加（5 秒超时）与 `ProxyQualityTarget` 质量检测（目标编译期常量或组合根注入）；`test()` 签名跟上游 | 同文件 | 取上游探测主体，补回 `exit_geo` 叠加与质量段 |
| `Cargo.toml` | B | `chrono-tz` | `from_geo` 时区 | 取上游，补回 |

## Provider 执行路径

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `providers/openai/src/lib.rs` | B | `mod cf_cookie_pool; fork_account_ticket; pool_overview; route_pair; turn_state_auto_hunt; turn_state_mint; turn_state_pin; ws_warm_pool;` | 各同名文件 | 取上游，补回 8 行 |
| `providers/openai/src/config.rs` | B | `revive_data_dir` / `cdk_data_dir` / `turn_state_data_dir`（`serde(skip)`，由 runtime 目录派生）；`auth.revive` 设置 | — | 取上游，补回 |
| `providers/openai/src/admin.rs` | B+C | `turn_state_pins`、`auto_hunt` 字段；`ProviderAdmin` 的 turn-state 遍历 / 打票 / 测智台 / 票据登录方法实现；`MintError::AccountExpired` 文案 | `turn_state_mint.rs`、`turn_state_auto_hunt.rs`、`fork_account_ticket.rs` | 取上游后补回 |
| `providers/openai/src/provider/workers.rs` | B | `openai-oauth-revive`（60 秒）、`openai-cloud-mint`（20 秒）、`openai-ws-warm-pool`（20 秒）三组常量 | `provider/mod.rs` 注册 | 取上游，补回；总 worker 数 = 上游 8 + 3 |
| `providers/openai/src/provider/mod.rs` | B+A | `mod fork_served;`；turn-state-pin / cloud-mint / ws-warm-pool 服务注入；diagnostic-egress 3 处（归因实际出口、临时出口探测不回写账号、`egress_account` 叠加） | `provider/fork_served.rs` | 取上游，补回 |
| `providers/openai/src/provider/execution.rs` | A+C | `with_outbound_proxy` 归因 1 处；9 处 `served-mismatch` 调用（`served_watch` 建立、`routed`、两处 `observe` + `annotate`、`observation_state` 后 `annotate`、两处 `completed`、两处 `log_patch`）；密文恢复在 WS 原始 JSON 逐帧转发下先攒住 `created` / `in_progress` 等空前导帧，首个非前导帧或终态再放行，被拒绝那次的前导帧作废 | `fork_served.rs`、`route_pair.rs`、`encrypted_content.rs` | 取上游后补回。`observe` 在 metadata 合并之后、`attach_openai_session_update` 之前；`completed` 与 `pin.completed` 同条件；不恢复预输出换号重试 |
| `providers/openai/src/provider/encrypted_content.rs` | — | fork 自有：明确拒绝后的密文恢复与有界缓存 | 本文件 | 保留；上游若新增同名模块需改名 |
| `providers/openai/src/provider/observation.rs` | B | `fork_metadata` 字段及初始化；`provider_metadata` 里 `metadata.extend(..)` | `fork_served.rs::annotate` | 取上游，补 3 处 |
| `providers/openai/src/provider/failure.rs` | B | `CodexWebSocketExchangeError::WarmUnavailable` 的分类与短暂退避重试 | `ws_warm_pool.rs` 严格模式 | 取上游，补回 |
| `providers/openai/src/credential/mod.rs`、`types.rs`、`security.rs`、`cookie.rs`、`admin.rs` | B | `mod cdk; mod revive; pub(crate) mod ticket_login;` 与 `pub use`；`turn_state_pin` 凭据字段及脱敏 / 合并时保留；路由 cookie `__cflb` / `__oailb` 白名单与 `with_loopback_upstream`；revive / guanlan-cdk 服务注入与 CDK 文档展开 | `credential/{cdk,ticket_login}.rs`、`credential/revive/` | 取上游，补回 |
| `providers/openai/src/credential/selector.rs` | A | cf-cookie-pool / request-log 记录块（到 `request_log::record` 为止）；`drop_route_pair`；`capture_response_cookies` 里 `captured_expiry(..)`（`__oailb` 按 JWT `exp`） | `cf_cookie_pool.rs`、`route_pair.rs`、`gateway-core::request_log` | 取上游，补回 3 处；会话续期用上游 `admit_session`，不补 fork 续期 |
| `providers/openai/src/transport/mod.rs`、`client.rs` | B | `mod warm_connection; pub use WarmConnectionApproval`；`egress_key` 放宽为 `pub(crate)`、`pool_egress_key` | `transport/warm_connection.rs` | 取上游，补回 |
| `providers/openai/src/transport/canonical.rs`、`response_meta.rs` | B | `declared_models()`、初始 HTTP 头与逐事件累积的 mismatch 事实；`has_model_mismatch` / `event_has_model_mismatch`（所有模型头及多值分别对照） | canonical decoder、WS reducer / stream | 取上游，补回；不能只用展示用的 `effective_model` |
| `providers/openai/src/transport/client_sse.rs` | A | `.with_mint_route(request.minted_turn_state_route.as_deref())` | `turn_state_pin.rs` | 取上游，补回 1 行 |
| `providers/openai/src/transport/downstream/body.rs` | C | `normalize_codex_request_body` 里 fork 的 Codex 400 兼容：缺 `role` 的 `message` 补 `user`；丢弃空 namespace、保留命名空间（browser/container/python）及 `browser.xxx` 形式的工具 | 原文件；测试 `tests/transport/downstream/fork_body.rs` | 取上游后补回两段；上游自己处理了同类形状时改用上游 |
| `providers/openai/src/transport/protocol/responses.rs`、`websocket/model.rs`、`websocket/handshake.rs` | B | `discard_mismatched_connection`、`minted_turn_state_route`、`warm_approval`、`warm_verification_policy`；WS 请求侧 `requested_model`、`warm_verification`、`require_verified_warm`、`allow_warm_reuse` 及握手时的初始化；`route_pair: None` | `turn_state_pin.rs`、`warm_connection.rs` | 取上游，补回控制字段，不写入上游正文 |
| `providers/openai/src/transport/websocket/mod.rs`、`error.rs` | B | `WARM_CONVERSATION_PREFIX` 导出；`WarmUnavailable` 错误 | `ws_warm_pool.rs` | 取上游，补回 |
| `providers/openai/src/transport/websocket/coordinator.rs` | A | 严格模式只走已验证候选 / 有限等待 warmer 发布（2 处）；握手后给 `metadata.route_pair` 赋值并核对云端票路由，不符则 discard | `route_pair.rs::RoutePairRef::handshake`、`warm_connection.rs` | 取上游，补回 |
| `providers/openai/src/transport/websocket/pool/mod.rs` | B+C | `warm_reuse: AtomicBool`；观测 / 驱逐 / 复探接口段（供 `ws_warm_pool.rs`、`pool_overview.rs`）；`acquire` 在上游入参之后追加领养事实形参（`#[expect(too_many_arguments)]`）；过期判定叠加保活证明寿命 / 驳回与票据校验；无会话连接时优先领养、严格模式不新拨 | `warm_connection.rs` | 取上游 acquire 主体，补回形参与两个分支 |
| `providers/openai/src/transport/websocket/pool/state.rs`、`supervisor.rs` | B | 云端票路由作为独立池键约束（3 处）；`CodexWebSocketConnectionMetadata.route_pair`；`warm_reuse` 透传 | `route_pair.rs` | 取上游，补字段 |
| `providers/openai/src/transport/websocket/exchange/{mod,reducer,stream}.rs` | A+B | 响应元数据的 `route_pair` / `discard_connection` / `served_mismatch`；reducer 的 `body_model` / `header_model_mismatch`（头与正文分别比）；`ServedModelMismatch` 丢弃原因与终态归还前的对照分支 | — | 取上游，补回；守卫分支排在正常 `Completed \| Interrupted` 分支之前 |
| `providers/openai/src/turn_state_mint.rs`、`ws_warm_pool.rs` | A | 打票与预热各一处 `is_expired` 跳过；`read_verdict` 同时对照 metadata 头与正文 `response.model`，声明了别的模型或没读到终态不算满血，流必须读到结束才归还连接 | `fork_account_ticket.rs`、`warm_connection.rs` | fork 自有文件，随上游 WS 交换 / 原始 JSON 转发接口适配 |
| `providers/xai/src/provider/mod.rs` | A | `.with_outbound_proxy(selected.binding().outbound_proxy())` | 归因 | 取上游，补回 1 行 |
| `providers/xai/src/transport/canonical.rs` | C | `Started` 带 `with_observed_model`；计价身份取发送模型并独立保留 `calculated`（双费用口径） | 计量 | 取上游后按双费用口径补回 |

## 测试

fork 测试模块命名 `fork_<主题>.rs`，和被测的上游测试文件同目录；父模块用带 `// fork:` 标记的 `mod` 行声明。
上游测试文件里不写 fork 用例；`src` 里不放 `#[cfg(test)]`（上游 e0a02251 起由
`workspace_production_files_have_no_hidden_modules_or_test_hooks` 机器校验，连 `cfg(test)` 字样都不能出现）。
测试树镜像生产模块树（`integration_tests_mirror_production_module_tree`）：`src/foo/bar.rs` 的用例放
`tests/foo/bar.rs` 或 `tests/foo/fork_bar.rs`，根级模块只能放 `tests/<同名>.rs`。

### 生产源文件的测试缝（doc(hidden) 再导出）

只能测私有纯函数时，照上游 `jitter.rs` 的做法：项标 `#[doc(hidden)] pub`，经 `lib.rs`（或 `pub mod` 的 `mod.rs`）
`#[doc(hidden)] pub use` 再导出给 `tests/` 用。不改行为、不加 feature、不用 `#[path]`。

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `gateway-admin/src/lib.rs`、`use_case/mod.rs` | B | `pub use use_case::{turn_state_hunt::{FailureClass as TurnStateHuntFailureClass, ephemeral_proxy_url}, usage_projection_cache::{…}}`；两个 fork 模块改 `pub(crate) mod` | `tests/use_case/{turn_state_hunt,usage_projection_cache}.rs` | 取上游，补回 |
| `gateway-core/src/engine/probe.rs`、`request_log.rs` | B | `AccountProbeExecution::from_trace` 与 `CAP` 放宽为 `#[doc(hidden)] pub` | `tests/engine/probe.rs`、`tests/request_log.rs` | 取上游，补回 |
| `providers/openai/src/lib.rs` | B | `cf_cookie_pool::{BorrowedCf, CfCookiePool}`、`provider::{MismatchDisposition, cfbm_issued_ttl_seconds, mismatch_disposition, served_mismatch_error}`、`turn_state_mint::{MintError, mint_http_client, mint_proxy}`、`ws_warm_pool::{Verdict as WarmPoolVerdict, WarmPoolService, answer_matches, disagreeing_model, event_declared_models, inherited_approval, verification_valid_for}` 的 hidden 再导出 | `tests/{cf_cookie_pool,turn_state_mint,ws_warm_pool}.rs`、`tests/provider/{execution,fork_served}.rs` | 取上游，补回 |
| `providers/openai/src/provider/{mod,execution}.rs` | B | `cfbm_issued_ttl_seconds`、`served_mismatch_error` 放宽为 `pub` 并在 `mod.rs` hidden 再导出 | `tests/provider/execution.rs` | 取上游，补回 |
| `providers/openai/src/transport/mod.rs` | B | `pub use warm_connection::{WarmVerificationContext, WarmVerificationSnapshot, WarmVerificationStatus}`（hidden）；`warm_connection.rs` 的 `publish_rechecked / permits_request / snapshot / needs_reprobe / checked` 放宽为 `pub` | `tests/transport/warm_connection.rs` | 取上游，补回 |
| `providers/openai/src/transport/websocket/{mod,exchange/mod,exchange/reducer,exchange/stream}.rs` | B | `WebSocketTerminalKind`、`TerminalRelease`、`note_served_model`、`terminal_release` 放宽为 `pub` 并逐层 hidden 再导出到 `transport::websocket` | `tests/transport/websocket/exchange/stream.rs` | 取上游，补回 |
| `gateway-admin/src/use_case/mod.rs`、`turn-state/src/{lib,record}.rs` | B | 同一条规则也禁止 src 内联模块：`use_case::openai`（`OpenAiService` 窄 trait）与 `record::millis`（serde 毫秒）搬成文件模块，`mod.rs` / `lib.rs` 只留 `pub mod openai;` / `mod millis;` | `use_case/openai.rs`、`turn-state/src/millis.rs` | 取上游，补回 |

### gateway-core / gateway-host / 插件 runtime

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `gateway-core/tests/routing/mod.rs` | D | `mod fork_request_log;` | `routing/fork_request_log.rs`（设置重编译与 rebase 的日志门控） | 取上游，补 1 行 |
| `gateway-core/tests/engine/coordinator.rs` | D+B | `FinalState` / `StoreState` / `FakeStore` / `Script` / `ScriptedProvider` 及字段放宽为 `pub(super)`；`billing` 字段及赋值；`Script::AttributedStream` 变体及分支 | `engine/fork_coordinator.rs` | 取上游，补可见性、`billing` 与 `AttributedStream` |
| `gateway-core/tests/engine/execution/mod.rs` | D | `ProbeModeProvider` 与测智台 `mode` 用例（待迁入 fork 文件） | — | 取上游后搬到 `engine/fork_execution.rs` |
| `gateway-core/tests/engine/mod.rs`、`tests/main.rs` | D | `mod fork_coordinator; mod fork_provider; mod probe;`；`mod request_log;` | `engine/probe.rs`、`tests/request_log.rs` | 取上游，补回 |
| `gateway-host/tests/outbound/mod.rs` | D | `mod fork_proxy_probe;` | `outbound/fork_proxy_probe.rs` | 取上游，补 1 行 |
| `gateway-plugin/runtime/tests/support/environment.rs` | D | `AdminRuntimePorts.fork` 独占临时目录 | — | 取上游，补回 |

### gateway-admin

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `tests/use_case/mod.rs` | D | fork 模块块（`fork_accounts`、`fork_proxies`、`public_import`、`turn_state_hunt`、`usage_projection_cache`）；harness 的 `fork` 运行目录字段；`proxy_probe` 构造方法 | — | 取上游，补回 |
| `tests/use_case/accounts/mod.rs` | D+C | `FakeProviderAdmin.fork` / `FakeAccountStore.fork` 字段与 4 个 `turn_state_hunt_*` 方法、`mutate_account`、`quota_window_queries`、`quota_local_usage`、`accounts_service`、`TestBenchProbe` 与测智台用例、`commit_import` 记录 `reject_existing`、`request_log_enabled` 夹具 | `use_case/fork_accounts.rs` | 取上游后补回；用例迁入 fork 文件后只留夹具 |
| `tests/use_case/proxies.rs` | D | `fork: TestProxiesFork` 字段并委托 `list` / `get`；`exit_geo` / `quality` 夹具字段；槽位排队用例 | `use_case/fork_proxies.rs` | 取上游，补夹具 |
| `tests/use_case/{public_import,turn_state_hunt}.rs`、`tests/ops_report.rs`、`tests/model/{ops_report,proxies}.rs` | — | fork 自有测试 | 本文件 | 保留 |
| `tests/use_case/account_groups.rs` | D | `FakeGroupStore`、`group_id` 的 `pub(super)` | 免登录导入用例 | 取上游，补可见性 |
| `tests/use_case/{settings,freeze_recovery}.rs`、`observability/mod.rs`、`plugins/distribution.rs` | D | `request_log_enabled` / `quality: None` 夹具字段 | — | 取上游，补字段 |
| `tests/model/mod.rs`、`tests/main.rs` | D | `mod account_tickets; mod ops_report; mod proxies; mod fork_quota_forecast*;`；`mod ticket_cipher;`；`billing: Default::default()` | `model/account_tickets.rs`、`tests/ticket_cipher.rs` | 取上游，补回 |
| `tests/model/quota_forecast.rs`、`quota_forecast_sampling.rs` | D | 辅助函数 `pub(super)`；`billing` / `curve` 夹具字段 | `model/fork_quota_forecast*.rs` | 取上游，补回 |
| `tests/use_case/credentials/openai.rs`、`credentials/mod.rs` | D | `service` 的 `pub(super)`；`mod fork_openai;` | `credentials/fork_openai.rs` | 取上游，补回 |

### providers/openai、providers/xai

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `openai/tests/main.rs`、`tests/provider/mod.rs` | D | `mod cf_cookie_pool; turn_state_mint; turn_state_pin; ws_warm_pool;`；`mod execution; fork_served;` | `tests/{cf_cookie_pool,turn_state_mint,turn_state_pin,ws_warm_pool}.rs`、`tests/provider/{execution,fork_served}.rs` | 取上游，补声明组 |
| `openai/tests/provider/contract/mod.rs` | D | `mod fork_diagnostic_egress; fork_encrypted_content; fork_served_mismatch; fork_turn_state_pin;`；2 处 `outbound_proxy_endpoint` 断言 | 同名文件 | 取上游，补声明组和 2 处断言 |
| `openai/tests/credential/contract/mod.rs` | D | `mod fork_route_pair;` | 同名文件 | 取上游，补 1 行 |
| `openai/tests/credential/mod.rs`、`transport/mod.rs`、`transport/websocket/mod.rs` | D | `mod cdk; fork_cookie; revive;`；`mod fork_billing_identity; fork_ws_warm_pool; warm_connection;`；`mod exchange; mint_route;`；`downstream/mod.rs` 的 `mod fork_body;`；WS 夹具不写 `response.model`、模型名大小写变体 | `credential/{cdk,fork_cookie}.rs`、`credential/revive/`、`transport/{fork_billing_identity,fork_ws_warm_pool,warm_connection}.rs`、`transport/downstream/fork_body.rs`、`websocket/{exchange/stream,mint_route}.rs` | 取上游，补声明组与夹具注释 |
| `openai/tests/transport/websocket/metadata.rs`、`transport/latency.rs` | D | `metadata_round_model`；换模型连接不回池的用例（待迁入 fork 文件） | — | 取上游后搬到 `fork_*` |
| `openai/tests/admin.rs` | D+C | worker 数断言 11（上游 8 + 3）；`oauth_transport_settings…` 只断言 `transport`；7 处 `pub(crate)`；`mint_publication` 模块；账号已过期不打票用例 | `tests/turn_state_pin.rs::admin_rotation`、本文件 | 取上游，worker 数改成「上游数 + 3」，补断言收窄、可见性，用例迁入 fork 文件 |
| `openai/tests/support.rs`、`config.rs`、`credential/types.rs` | D | `fail_credential_writes`、`ticket_expiry`、`set_turn_state_pin`；4 处 revive 配置断言；`turn_state_pin: None` | 原文件 | 取上游后补回 |
| `xai/tests/provider/contract/mod.rs`、`provider/mod.rs` | D | `StubSelector` 等 `pub(super)`；`proxy` 夹具字段与 `with_outbound_proxy`；`mod fork_attribution;` | `provider/fork_attribution.rs` | 取上游，补回 |
| `xai/tests/transport/canonical.rs` | C | 4 处断言按双费用口径（上游报告费用不抹掉本地 `calculated`） | 原文件 | 取上游后按双费用口径重新调整断言 |

### gateway-store、gateway-api

| 文件 | 类型 | fork 留下了什么 | 实现在哪 | 合并策略 |
| --- | --- | --- | --- | --- |
| `gateway-store/tests/postgres/mod.rs` | D | `mod fork_execution_billing; fork_proxy_quality; fork_request_log_settings;` | 同名文件 | 取上游，补 3 行 |
| `gateway-store/tests/postgres/connection.rs` | D | 迁移表清单里 `account_tickets`、`model_request_billing`、`outbound_proxy_quality` | — | 取上游，补 3 行 |
| `gateway-store/tests/postgres/runtime_settings.rs` | D | 日志两字段夹具；快照期望并入 `9005` 两列 | `postgres/fork_request_log_settings.rs`（含已有 9xxx 后补跑上游迁移的升级验证） | 取上游，补回 |
| `gateway-store/tests/postgres/provider_accounts/mod.rs` | D | `mod fork_billing; fork_expired_status; fork_turn_state;`；3 处状态筛选 `.into()`；`exit_geo: None` | 同名文件 | 取上游，补回 |
| `gateway-store/tests/postgres/proxies.rs` | D | `exit_geo: None`；`context()` / `success()` 的 `pub(super)` | `postgres/fork_proxy_quality.rs` | 取上游，补回 |
| `gateway-store/tests/postgres/execution/mod.rs` | D | 2 处 `billing: Default::default()`；`successful_core_finalization` / `seed_running_request` 的 `pub(super)` | `postgres/fork_execution_billing.rs` | 取上游，补回 |
| `gateway-api/tests/main.rs` | D | `#![recursion_limit = "256"]`（设置合同多两个键） | — | 取上游，补回 |
| `gateway-api/tests/admin/mod.rs` | D | `mod fork_proxy_quality; public_import; turn_state;`；harness `fork` 运行目录；`request_log_*` 透传；`summary.expired: 0` | `admin/fork_proxy_quality.rs`、`admin/{public_import,turn_state}.rs` | 取上游，补回 |
| `gateway-api/tests/admin/accounts/mod.rs` | D | `mod fork_presenter; mod fork_turn_state_wire;`；2 处 `.into()`；`estimated_quota_usd_display` / `billing` 夹具字段 | 同名文件 | 取上游，补回 |
| `gateway-api/tests/admin/accounts/handlers.rs` | C | 上游「rotate 路由不存在」用例改成 `fork_rotation_route_stays_exposed_and_validates_its_material`；用量投影 20 秒 SWR 缓存下每轮用新 fixture；`billing` 夹具 | 原文件 | 取上游后改回 fork 用例 |
| `gateway-api/tests/admin/accounts/presenter.rs` | D | 4 个预测字段（`window_start_at` / `curve` / `burn_percent_per_hour` / `exhaustion`） | `accounts/fork_presenter.rs` | 取上游，补字段 |
| `gateway-api/tests/admin/proxies.rs` | D+C | `MemoryProxies` 第二个槽位保存质量报告、`record_quality` / `quality_report`、`quality: None`、`exit_geo`；`request()` 的 `pub(super)` | `admin/fork_proxy_quality.rs` | 取上游替身，把质量部分补回同一个 impl |
| `gateway-api/tests/admin/settings.rs` | D | `requestLogEnabled` / `requestLogTestKeyId` 期望与夹具 | — | 取上游，补回 |
| `gateway-api/tests/{health,openai/mod,openai/auth,openai/live/websocket,openai/responses/websocket/**}.rs` | D | `initialize` 的 `turn_state` 实参 `None`（7 处） | — | 取上游，补回 |
| `gateway-api/tests/architecture.rs` | — | 无。上游已改为只检查 manifest 依赖与管理路由约定，不再维护源码 / 测试清单白名单 | — | 直接取上游；新增 fork 文件不需要登记 |
