# fork 管理 API 增补

本文只记录本 fork 相对上游 [`docs/api.md`](../api.md) 新增或改变的管理接口与语义；上游已有的内容以
上游文档为准。合并上游时 `docs/api.md` 直接取上游版本，fork 的变化只改本文。

## 账号接口增补

| 方法 | 路径 | 参数 | 说明 |
| --- | --- | --- | --- |
| `POST` | `/api/admin/accounts/import` | `{ provider, data, settings?, outboundProxyId? }` | 导入或按上游身份更新账号，可同时应用调度、分组设置与默认代理。OpenAI 的 `data` 可以是观澜 CDK 文档 `{ cdks: ["CDK-..."] }`，网关会兑换并导入已签名账号包 |
| `POST` | `/api/admin/accounts/mint-turn-state` | `{ accountId, models? }` | 云端打票：向 relay 铸票并钉成账号级 state、把路由 cookie 对写进凭据；`models` 省略时用 turn-state 设置里的列表。返回 `{ gateway, attempts, observeOnly, pairWritten, tickets:[{model,length,servedModel,expiresAt}] }`，不含票值 |
| `GET` | `/api/admin/accounts/turn-state-hunt` | `accountId`、`modelId`、`attempts`（1–200，默认 5）、`includeDirect`、`proxyId`（可选，只遍历这一个代理） | 通过 SSE 遍历已测试通过的代理找符合长度规则的 state；命中后绑定该代理并钉住 state |
| `GET` | `/api/admin/accounts/turn-state-auto-hunt` | `accountId`、`modelId`、`templateProxyId`（轮换代理模板）、`countries`（逗号，US/JP/DE/PH）、`staticProxyIds`（逗号）、`maxIps`（1–2000） | 从轮换代理模板即时生成多国临时出口反复撞（每个 IP 打 1 次），命中后改绑到静态池里账号数最少的出口、并按该静态出口的指纹钉住 state；事件形状与 `turn-state-hunt` 一致 |
| `GET` | `/api/admin/accounts/ticket` | `accountId` | 读取账号成本、到期与票据摘要（票据只回打码邮箱） |
| `POST` | `/api/admin/accounts/ticket` | `{ accountId, purchaseAmount?, purchaseCurrency?, purchasedAt?, expiresAt?, ticket? }` | 保存成本、到期与票据；`ticket` 省略表示保留原票据 |
| `POST` | `/api/admin/accounts/ticket/restore` | `{ accountId }` | 用已存票据登录换回令牌并恢复账号 |
| `POST` | `/api/admin/accounts/rotate` | 见下文「固定自身 state」「观澜复活」「自动续期」 | fork 保留的凭据轮换入口（上游已移除；普通连接编辑走 `/accounts/update`） |

账号列表额外支持 `status=expired`：票据 `expiresAt` 已到且账号已不能调度（五态不是正常 / 限流）的账号归入
「已过期」，默认列表与 `summary.total` 不含它们，`summary.expired` 单独计数。

### 账号用量与费用来源

账号列表、详情的 `usage.billing` 和逐模型 `usage.models[].billing` 使用相同时间窗口，提供以下 USD 字段：

| 字段 | 语义 |
| --- | --- |
| `modelPriceAmountUsd` / `modelPriceAmountUsdDisplay` | 按计费模型价格和已记录用量计算的金额；不是上游实际扣费 |
| `upstreamCostAmountUsd` / `upstreamCostAmountUsdDisplay` | 仅聚合上游明确返回的费用；未返回时为 `null` / “未提供”，明确零值为 `0` |
| `modelPriceCount` / `upstreamCostCount` | 对应费用已提供的请求数；未覆盖全部请求时展示部分覆盖数 |
| `differenceAmountUsd` / `differenceAmountUsdDisplay` | 模型计价减真实上游费用；仅当同一组所有请求的两类 USD 费用均已提供时计算，否则为 `null` / “不可计算” |

`usage.models[]` 按 `requestedModelId`、`upstreamModelId`、`responseModel`、`billingModel` 四元组分行；
`key` 为该组合的稳定行标识。分别表示请求模型、路由模型、上游实际响应模型、实际采用的价格模型。
请求模型与任一已知路由、响应或计费模型不一致时，`mismatch=true`。未知身份为 `null`，不把协议回退
模型或历史请求名称猜作响应/计费模型。相同 Astra 请求、不同 Luna 响应或计价不会合并成一行。

旧 `costs` 字段保留其原有的来源混合聚合，仅供兼容；账号费用展示应使用 `billing`。
`billingAmountUsd` 是逐模型计价金额的兼容字段。历史记录中明确为 `calculated` 的金额迁入模型计价列，
历史响应模型与计费模型保持未知；历史 `provider_reported` 金额可显示为真实费用，不反推模型计价。
上游未返回支持的明确费用字段时，系统无法确认其实际扣费；当前支持精确 USD ticks，站外消耗不在统计内。

### 用量窗口口径（与上游的差异）

- 已提供额度窗口、但没有边界完整且可归属到账号的周/月窗口时显示无数据；OAuth 账号尚无任何额度窗口时，
  `usage` 返回账号创建后仍保留的本地请求累计，标签为「本地累计」。API Key 账号保持相同的本地累计口径和
  「通用额度」标签。上游额度未知不影响已有消费金额的展示，金额缺失保持未知，已知零金额与未知分开呈现。
- 账号列表「用量」列在 Token 汇总外同时展示本机记录的 USD 已用金额；当前额度窗口 `usedPercent` 大于 0 时，
  再按 `已用 × 100 / usedPercent` 给出近似额度，供对照官方使用率，不代表 OpenAI 账单或站外消耗。

### 周/月额度预测增补字段

- `windowStartAt` / `curve`：源窗口起点，以及窗口内当前连续段的已用比例观测
  `[{ observedAt, usedPercent }]`（按时间升序，末点为当前额度快照）。曲线与能否预测无关，
  样本不足时仍返回；额度回落（上游重置）之前的旧段不返回。窗口过期或边界无效时为 `null` / `[]`。
- `burnPercentPerHour` / `burnPercentPerHourDisplay`：与容量估算同一配对样本的消耗速率
  （`sampledPercent / 采样时长`，百分点每小时）；不满足 5 个百分点门槛时为 `null` / `—`。
- `exhaustion`：源窗口的耗尽结论，不随目标周期折算。`kind: "reached"` 表示上游已报告用尽（事实，
  不需要速率）；`"at"` 附带按上述速率线性外推的 `at` / `atDisplay`；`"afterReset"` 表示按当前速率
  在 `source.resetAt` 之前不会用尽。无法给出结论时为 `null`。外推假设近期速率不变，仅供参考。

## 代理管理增补

| 方法 | 路径 | 请求 | 返回 |
| --- | --- | --- | --- |
| `POST` | `/api/admin/proxies/quality-check` | `{ id, revision }` | `{ record, report }` |
| `GET` | `/api/admin/proxies/quality-report` | `id` | `{ report }`，从未检测时 `report` 为 `null` |
| `POST` | `/api/admin/proxies/batch-create` | `{ items: [{ name?, proxyUrl }] }`（1-200 条） | `{ created, skipped, configRevision }` |
| `POST` | `/api/admin/proxies/batch-delete` | `{ items: [{ id, revision }] }`（1-200 条） | `{ deletedIds, skipped, configRevision }` |

与上游的差异：

- `record.lastTest` 额外带 `exitGeo`，`record` 额外带 `quality`（见下）。
- 测试与质量检测共用每进程 4 个槽位；槽位占满时请求排队最多 20 秒，仍未取得槽位才返回 429（上游立即返回 429），
  因此客户端超时应大于「排队 + 探测」。
- 连通性测试成功后再经同一代理查询一次出口地区（5 秒超时），得到 `exitGeo`；手动位置模式下据此回填请求位置。

`exitGeo` 为 `null` 或 `{ country, countryCode, region, city }`，由出口 IP 推断，仅用于展示，与用于
Responses 的 `location` 无关。地区查询是尽力而为：失败、被限流或返回不可信内容时 `exitGeo` 为 `null`，
不改变 `success`，也不计入 `latencyMs`。国家代码为两位大写字母；`region`、`city` 可为 `null`。

`quality` 为 `null` 或最近一次质量检测的结论 `{ score, grade, status, summary, checkedAt }`，随列表下发；
逐项明细只在 `report` 中返回。`report` 在结论之外包含 `exitIp`、`exitGeo`、`baseLatencyMs`、
`passedCount`、`warnCount`、`failedCount`、`challengeCount` 和
`items: [{ target, status, httpStatus, latencyMs, message, cfRay }]`。

质量检测先做一次完整的连通性测试（`target: "base_connectivity"`，其结果同时写入 `lastTest`），
连通后经同一代理并发请求固定的上游目标；连通失败时不再请求上游。目标为编译期常量，不接受请求传入：
`chatgpt`（Codex 后端）、`openai_auth`（登录与令牌）、`openai_api`、`xai`。请求不带凭据、不跟随重定向，
每个目标 15 秒超时：

- 返回 2xx 或该目标预期的无凭据状态（如 401）记为 `pass`，表示目标可达；
- 403 / 429 且带 `cf-mitigated: challenge` 响应头或挑战页特征记为 `challenge`，并附 `cfRay`；
- 其余 429 记为 `warn`；其他状态码、超时或连接失败记为 `fail`。

`score = 100 − 10×warn − 22×fail − 30×challenge`（下限 0）；`grade` 为 A（≥90）、B（≥75）、C（≥60）、
D（≥40）、F。`status` 按 `challenge` > `failed` > `warn` > `healthy` 取最严重的一项。
评分通过不表示账号权限或额度可用。连接配置改变时，质量结论与测试结果一起清除；普通测试不清除质量结论。

批量添加逐条处理，单条失败不影响其余条目：地址不合法（`reference` 为「第 N 条」）或与已有代理重复
（`reference` 为脱敏端点）计入 `skipped: [{ reference, reason }]`，响应不回显任何凭据。`name` 省略时取
脱敏端点的 `host:port`。批量删除同样逐条处理：仍被账号使用、版本过期或已不存在的代理计入 `skipped`
（`reference` 为代理 ID）。没有任何条目成功时 `configRevision` 为 `null`。
新建的代理尚未测试，绑定账号前仍需通过连接测试。

### 免登录账号导入

管理员在「系统设置」开启后，把 `https://<host>/import/<token>` 发给上游；对方无需账号密码即可导入 sub2api
格式的 OpenAI 账号。入口默认关闭，配置保存在 runtime 数据目录的 `public_import/config.json`，蓝绿槽位共享，
不进入数据库和备份。

> 当前实现已改为**按号商多配置**：每个号商一条独立配置（名称、专属令牌 / 链接、目标分组、WS 保活、有效期），
> 可新增、删除、单独更换链接；导入的账号把号商名写进备注。下表描述的单配置字段在每条号商配置上各有一份。

| 方法 | 路径 | 鉴权 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/public-import` | 管理员 | 返回 `{ enabled, token, groupIds, pinTurnState, expiresAt, updatedAt }`，首次读取时生成令牌 |
| `POST` | `/api/admin/public-import/update` | 管理员 | `{ enabled, groupIds, pinTurnState, expiresAt }`；开启时至少一个已存在的分组。`expiresAt` 必填，RFC 3339 时间或 `null`（长期有效），开启时必须晚于当前时间 |
| `POST` | `/api/admin/public-import/rotate-token` | 管理员 | 更换令牌，旧链接立即失效；不改变 `expiresAt` |
| `GET` | `/api/public-import/entry` | `X-Import-Token` | 返回 `{ groupNames, pinTurnState, expiresAt, maxAccounts }` |
| `POST` | `/api/public-import/accounts` | `X-Import-Token` | `{ data }`，请求体上限 8 MiB |

令牌缺失、错误、已过期和入口关闭统一返回 404，不区分原因。有效期在每次请求时校验，到期无需任何后台任务。`data` 接受 sub2api 导出（含 `{ code, message, data }` 响应信封）、
`accounts` 数组或单账号文档，单次最多 200 个账号，不接受观澜 CDK。服务端把文档拆成单账号条目逐个导入：

- 出站代理从「已通过连通性测试」的已保存代理中逐账号随机抽取；文档自带的 `proxies`、`proxy_key`、
  `outboundProxyUrl` 一律丢弃。没有可用代理时整次请求返回 409，不回退直连；
- 账号以启用状态、默认权重和并发加入配置的目标分组，审计主体为 `system`，`request_id` 为本次请求 ID；
- `pinTurnState` 开启时，导入成功的账号随后开启「固定自身 state」。API Key 账号不支持该开关，
  条目仍算导入成功，`statePinned` 为 `false`。

响应为 `{ total, imported, failed, items: [{ index, name, status, importedAccounts, proxyName, statePinned, message }] }`，
`status` 为 `imported` 或 `failed`，不回显账号 ID 和凭据。单个条目失败不影响其余条目。
管理端页面按单账号分批提交并展示进度；直接调用接口批量提交时，整次请求受部署的 `api.request_timeout_seconds` 约束。

## 固定自身 state、观澜复活与遍历代理

OpenAI OAuth 账号可在编辑页开启实验性的「固定自身 state」。通过 `rotate` 提交
`{ provider: "openai", accountId, pinTurnState: true | false, settings? }`，此分支不能混入 token、API Key
或外部 state。开关使用现有管理员鉴权、账号 CAS 和审计事务，保留凭据健康状态、错误及额度；
再次提交 `true` 表示重新捕获。普通账号更新和令牌刷新保留开关，其他 Provider / API Key 不支持。
详情的 `credentialConfiguration` 返回 `{ pinTurnState, maxAgeSeconds, turnStatePins, turnStateCaptureRule, guanlanReviveAvailable }`；每条缓存摘要仅含
`model`、`length`、`capturedAt`、`expiresAt` 和 `hits`，不包含原始 state 或令牌。

`guanlanReviveAvailable` 表示该 OAuth 账号存在可用的观澜签名导入原文。管理员可向 `rotate` 提交
`{ provider: "openai", accountId, guanlanRevive: true }` 手动复活，不可混入其他凭据、state 或设置字段。
服务端使用归档签名文件完成观澜验证和恢复，只将返回结果中匹配目标身份的凭据通过原有 CAS / 审计事务写回。
没有匹配恢复结果、签名记录缺失、任务执行中或失败冷却期间返回错误，不将普通「恢复状态」当作复活。
手动任务与自动复活互斥；失败使用现有 30 分钟冷却。该请求可能包含两段最长各 30 分钟的上游轮询，
调用方及反向代理需要容纳相应超时；请求失败后应先回读账号，不自动重放。手动复活不改变账号调度开关。

开启后，从该账号普通生成请求的成功完整响应中捕获首个符合套餐规则的候选，按账号、上游模型及客户端密钥隔离。
`turnStateCaptureRule` 返回 `defaultLength`（字节数或 null）和 `modelLengths`（上游模型 ID 到字节数）。
Team / Business（含 `self_serve_business_prolite`、`self_serve_business_usage_based`）账号中，`gpt-5.5`、`gpt-5.6-sol`、`gpt-6-astra` 使用 332 字节，`gpt-5.6-terra` 使用
356 字节；其它模型暂不捕获。Pro 和其它套餐保留原有 292 字节规则。长度按原始 ASCII state 字节计算，
不解码或截断；缓存摘要 `length` 返回实际字节数，管理端以服务端规则展示筛选说明。
固定期内覆盖发往上游的 state，后续返回值不覆盖已固定值；失败、不完整响应、预热和连接测试不捕获。
默认关闭。单个候选的本地最长保留时间为 3600 秒，不随命中续期；这不是已验证的上游有效期。
到期、访问令牌改变、重新捕获或服务重启后等待新的候选，尚无候选时保持原有透传行为。
此功能偏离常规同轮粘性路由合同，state 长度不构成模型质量判断，也不保证减少 overload。

#### 遍历代理找 state

上游是否返回符合规则长度的 state 与出口有关。`GET /api/admin/accounts/turn-state-hunt` 对单个 OpenAI OAuth 账号
依次经每个 `lastTest.success` 为 true 的代理发真实上游请求（`includeDirect=true` 时再加直连），账号当前绑定的出口排最前，
每个出口最多 `attempts` 次。给出 `proxyId` 时只遍历该代理（不存在或未通过测试则 400，不会退回成遍历全部，也不再尝试直连）：轮换出口每次请求换一个 IP，值得单独打上百次，固定出口反复打同一个 IP 没有意义，所以管理端只在指定单个代理时放开到 200 次。自动续期保存的 `attempts` 上限仍为 20。探测只替换单次请求的出口，不改账号已保存的绑定，也不计入 Provider 熔断和账号探测失败事实。命中收尾时若发现令牌刚好刷新（凭据变了），不整轮中止，而是重新取票、丢弃这次命中、继续用新票撞下一个出口，最多 3 次——繁忙的 Business 号令牌刷新频繁，整轮因此中止会让它永远绑不上 state。
要求该账号已开启并保存 `pinTurnState`，且模型在 `turnStateCaptureRule` 中有长度规则，否则直接返回 400；同一账号同时只允许一个遍历（409）。
这是会改状态的 GET（EventSource 只能发 GET），请求必须带 `Accept: text/event-stream`，且浏览器请求的
`Sec-Fetch-Site` 必须是 `same-origin`（挡掉同站兄弟子域），否则返回 400。

首次命中即停止：先核对凭据绑定未变，再把账号绑定到该出口（等同批量更新的 `outboundProxyId`，已绑定则跳过），最后把该 state
钉为账号级 state。**账号级 state 只对经同一出口发出的请求生效**：账号之后被改绑到别的出口（包括绑定与钉住之间
的并发改绑）就不再使用，续期会在新出口上重新找。绑定以回读到的账号出口为准：代理在探测后被修改（`egress_changed`）或账号最终没有落在
探测过的出口上（`bind_mismatch`）时不钉。账号级 state 对该账号该模型的全部客户端密钥生效，并**替换**该模型已有的全部固定（旧值来自换绑前的出口）；
被动捕获仍然永不覆盖。`turnStatePins` 摘要以 `scope: "account" | "client"` 区分二者，寿命同为 3600 秒且不续期。
提交边界是 `hit` 事件进入发送队列：此后绑定与钉住在服务端独立完成，不受页面断开影响；此前连接已断开的，
即使在途请求随后命中，账号也不被改动。SSE 无法确认事件是否已经送达浏览器，所以「取消」与「命中」几乎同时发生时
服务端可能已经提交；管理端在取消后会按服务端的实际状态刷新，以它为准。

事件为 `data:` JSON，以 `type` 区分：`hunt_start{model,expectedLength,attempts,proxies[]}`、`proxy_start`、
`attempt{proxyId,index,length,matched,error}`、`proxy_done{attempts,matched,skipped}`、`hit`、`bound{proxyId,changed}`、
`pinned{model,length,expiresAt}`、`hunt_complete{success,requests}`、`error{code,message}`。直连的 `proxyId` 为 null。
`length` 仅供筛选；state 的值及其任何摘要永不输出，也不写入日志。
失败按 Provider 的原始分类判断：凭据失效、无权限/封号、额度耗尽、限流、模型不支持、请求不合法视为账号级失败并中止
整个遍历（`account_rejected`）；网关本地的账号存储/租约/凭据故障（`system_error`）同样中止——它与出口无关。
上游明确拒绝该模型的容量多为秒级过载：每次等待 3 秒后继续（同一出口还有尝试次数就地再试，否则该出口以
`skipped: "capacity"` 结束并换下一个），跨出口连续 3 次才以 `upstream_capacity` 中止；期间任何一次请求拿到上游应答即清零。
只有传输失败、超时、Cloudflare 拦截、协议不合法才视为出口问题，同一出口连续两次即跳过。`attempt.error.message`
是按分类给出的固定文案，不含上游原文；`requests` 只统计真正发往上游的请求。
账号被线上流量占用而未发出的尝试不计次数，连续五次后中止。

**自动续期。** `POST /api/admin/accounts/rotate` 接受 `turnStateAutoHunt: { enabled, modelId, attempts, includeDirect }`
（`enabled: false` 关闭；不要同时提交 `pinTurnState`，重新提交开关会更换代次并作废已钉住的 state）。参数保存在运行数据目录
`turn_state/auto_hunt.json`（各实例共享），**不进账号凭据**——凭据 schema 拒绝未知字段，写进去会让回滚后的旧版本读不了该账号。
参数在凭据提交成功之后才写入（提交失败则不生效），读写在跨进程文件锁内完成；文件损坏时拒绝覆盖并停止续期，不会当成空表。
开启续期要求固定已开启，关闭「固定自身 state」时一并清除；详情的 `credentialConfiguration.turnStateAutoHunt` 返回当前参数或 null。
开启后服务端每 60 秒检查一次：该账号该模型的账号级 state 在本进程内缺失（含服务重启后）或将在 5 分钟内到期时，
以系统身份重新撞：**代理池里存在轮换代理模板（用户名含 `_area-` 的 smartproxy 式地址）时，续期走「自动撞」**——
从模板即时生成 US/JP/DE/PH 随机的临时出口（每轮最多 60 个 IP、每个打 1 次），命中后改绑到账号数最少的静态出口；
没有轮换模板时回退到遍历已存代理（当前绑定的出口排最前，续不上就继续打其它出口）。命中后照常换绑并替换旧 state。
整轮都未命中则 5 分钟后重试，上游拒绝账号则 15 分钟后重试，因上游无容量中止则 60 秒后重试（必须远短于 5 分钟的续期提前量，否则一次过载就会让 state 在下次重试前过期）。停用或凭据失效的账号不续期：轮到它时会按当时的事实和参数重新确认，
续期途中被停用会在下一个请求前停手（`account_unschedulable`，不计退避，重新启用后立即恢复）。
续期途中才关闭续期开关的，本轮（最多「出口数 × 次数」个请求）仍会走完。
state 只存在于进程内存，续期任务不加跨实例租约，每个实例各自维护。

## turn-state 模板与观测

`turn_state` crate 承载 Codex `X-Codex-Turn-State` 模板（按账号 × 模型分桶，落 `<runtime_data_dir>/turn_state/`，蓝绿实例共享）。管理接口只用 GET/POST 静态路径，票值永不返回。

| 方法 | 路径 | 请求 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/turn-state/settings` | 无 | 运行设置；`cloudMint.relayKey` 只回 `<set>`/空 |
| `POST` | `/api/admin/turn-state/settings/update` | 全部字段 + 可选 `cloudMint` | 整体替换并热生效；`cloudMint` 省略时保留现值，`relayKey` 为 `<set>` 时沿用已保存密钥；模板/受限长度表不能重叠 |
| `GET` | `/api/admin/turn-state/observations` | 无 | 按桶的正常/受限/未知/沉默与注入盲区计数、48 小时分时、长度直方图、最近 100 条事件 |
| `GET` | `/api/admin/turn-state/buckets` | `account?`、`model?` | 有效模板摘要（范围、长度、签发/到期、来源、网关、命中），不含值 |
| `POST` | `/api/admin/turn-state/buckets/clear` | `{ account, model? }` | 清除模板（内存与磁盘） |

设置字段：`ttlSeconds`（600–86400）、`injectMode`（`always` 有模板就注入；`replace-only` 只替换受限档长度的 state）、`dryRun`、`logDecisions`、`templateLengths`、`degradedLengths`（空表 = ≥200 字节可见 ASCII 下限规则）、`cloudMint { enabled, mode, observeOnly, relayUrl, relayKey, proxyUrl, gateway, ticketLen, ticketTtlSeconds, models, transport, cooldownSeconds, maxAttempts }`。
`mode=native`（默认）由 cpr 经账号绑定的代理直接向上游铸票（出口 = 该代理 IP），验收票长、`__oailb` 内嵌网关名与 `response.created` 的模型声明；`mode=relay` 交给 `deploy/cloud-mint/` 的 relay（出口 = relay 所在机器）；账号缺票时请求侧异步预热一次，后台每 20 秒对最近 10 分钟有流量的账号在票剩余不足 60 秒时续打。
