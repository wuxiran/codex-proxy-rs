# CPR 请求归因

`model_requests.diagnostic_trace_json.request_attribution` 使用现有原子终态写入，无新增数据库迁移。

| 字段 | 来源 |
| --- | --- |
| `requested_model` | 入口冻结的请求模型 |
| `route_model` | 最后一个通过路由与账号校验的 attempt 模型 |
| `response_model` | 与 billing 持久列共用的实际上游回显模型，不使用协议 fallback |
| `billing_model` | 与 billing 持久列共用的实际计价模型；只有真实费用而无计价身份时仍为 null |
| `provider_account_id` | 最后一个有效 attempt 的账号 |
| `outbound_proxy_endpoint` | 该 attempt 的脱敏代理端点；明确直连为 `direct`，未提供观测为 null |

路由、账号、代理在有效 attempt 建立时冻结。失败重试取走 `current` 后，如果下一次选号失败或请求中断，
终态仍保留最后有效 attempt 的归因，不因冷流已释放而变为 null。尚未建立有效 attempt 时这些字段为 null。
响应模型和计费模型复用 `ModelBillingObservation` 及其 attempt reset；丢弃尝试的模型或费用不会污染
后续成功尝试。请求、路由、响应和计费模型允许彼此不同，不因观察到费用事件而推断计费模型。

每次有效 attempt 在既有有界诊断时间线记录 `request.attempt_attribution`，携带 attemptIndex、路由模型、
账号和脱敏代理。时间线继续遵循既有事件容量与截断规则；终态归因独立于 current 和时间线事件淘汰。

Provider 使用类型化 `OutboundProxy` 填写元数据，在 Core 元数据入口调用其 `endpoint()` 统一剥离
用户名和密码，仅保留协议、host 和 port；不接收未验证的原始 endpoint 字符串。覆盖 OpenAI Responses、
Images/Search 与 xAI Provider。既有请求字段、费用来源、独立模型计价和两类费用结算语义保持不变。

行为测试位于镜像 `tests/engine/coordinator.rs`、`tests/engine/provider.rs`、两 Provider 的 `tests/provider/contract.rs`
和 Store 的 `tests/postgres/execution.rs`，覆盖终态、换号、费用独立性、脱敏与持久化。
