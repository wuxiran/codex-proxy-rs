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
| `POST` | `/api/admin/accounts/rotate` | 见下文「固定自身 state」「观澜复活」「按需补票」 | fork 保留的凭据轮换入口（上游已移除；普通连接编辑走 `/accounts/update`） |

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

OpenAI OAuth 账号在「票据管理 → 账号票与预热」启停票据与预热，账号编辑页不提供此开关。通过 `rotate` 提交
`{ provider: "openai", accountId, pinTurnState: true | false, settings? }`，此分支不能混入 token、API Key
或外部 state。开关使用现有管理员鉴权、账号 CAS 和审计事务，保留凭据健康状态、错误及额度；
再次提交 `true` 表示重新捕获。普通账号更新和令牌刷新保留开关，其他 Provider / API Key 不支持。
该页分别展示有效票和预热连接数，最近探针判定只描述最近一次探测，票据长度不用于推断模型能力。
详情的 `credentialConfiguration` 返回 `{ pinTurnState, maxAgeSeconds, turnStatePins, turnStateCaptureRule, guanlanReviveAvailable }`；每条缓存摘要仅含
`model`、`length`、`capturedAt`、`expiresAt` 和 `hits`，不包含原始 state 或令牌。

`guanlanReviveAvailable` 表示该 OAuth 账号存在可用的观澜签名导入原文。管理员可向 `rotate` 提交
`{ provider: "openai", accountId, guanlanRevive: true }` 手动复活，不可混入其他凭据、state 或设置字段。
服务端使用归档签名文件完成观澜验证和恢复，只将返回结果中匹配目标身份的凭据通过原有 CAS / 审计事务写回。
没有匹配恢复结果、签名记录缺失、任务执行中或失败冷却期间返回错误，不将普通「恢复状态」当作复活。
手动任务与自动复活互斥；失败使用现有 30 分钟冷却。该请求可能包含两段最长各 30 分钟的上游轮询，
调用方及反向代理需要容纳相应超时；请求失败后应先回读账号，不自动重放。手动复活不改变账号调度开关。

开启后，普通生成请求可从完整成功响应中捕获候选，按账号、模型及客户端密钥隔离，被动捕获不覆盖已有有效票。
票据合法性检查使用 ASCII 与最小长度，不再按套餐或模型精确匹配长度。`turnStateCaptureRule` 是兼容字段，
`defaultLength: 0` 表示无精确长度门，不能解释成要求零字节。上游票据记录只展示观测摘要，不推断是否入库。
注入按运行设置决定，默认 `fill-missing` 保留客户端自带当轮票。失败、不完整响应及原始诊断不进入被动捕获，
暖池验证后的发布遵循[连接预热](#连接预热)规则。被动票有效期默认 240 秒，不随命中续期，最终以票据到期时间为准。
到期、凭据绑定变化或重新捕获后重新获取候选，持久化的有效账号票可在重启后重新读取，尚无票时保留普通请求路径。

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
被动捕获仍然永不覆盖。`turnStatePins` 摘要以 `scope: "account" | "client"` 区分二者，寿命同为 `ttlSeconds`（默认 240 秒）且不续期。
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

**按需补票。** `POST /api/admin/accounts/rotate` 接受 `turnStateAutoHunt: { enabled, modelId, attempts, includeDirect }`
（`enabled: false` 关闭；不要同时提交 `pinTurnState`，重新提交开关会更换代次并作废已钉住的 state）。参数保存在运行数据目录
`turn_state/auto_hunt.json`（各实例共享），**不进账号凭据**——凭据 schema 拒绝未知字段，写进去会让回滚后的旧版本读不了该账号。
参数在凭据提交成功之后才写入（提交失败则不生效），读写在跨进程文件锁内完成；文件损坏时拒绝覆盖并停止续期，不会当成空表。
开启续期要求固定已开启，关闭「固定自身 state」时一并清除；详情的 `credentialConfiguration.turnStateAutoHunt` 返回当前参数或 null。
票不在到期前定时续。开启后服务端每 20 秒检查一次：该账号该模型最近一个 `ttlSeconds` 内有业务请求缺过票
（请求来时没有可用的 state），且账号级 state 此刻缺失或已过期时，以系统身份重新撞；闲置账号的票过期后不补，
下一个业务请求会不带票发出并留下缺票记录，随后的周期补上。缺票记录只在进程内存，重启后由下一个缺票的请求重新记上。
补票时：**代理池里存在轮换代理模板（用户名含 `_area-` 的 smartproxy 式地址）时，续期走「自动撞」**——
从模板即时生成 US/JP/DE/PH 随机的临时出口（每轮最多 60 个 IP、每个打 1 次），命中后改绑到账号数最少的静态出口；
没有轮换模板时回退到遍历已存代理（当前绑定的出口排最前，续不上就继续打其它出口）。命中后照常换绑并替换旧 state。
整轮都未命中则 5 分钟后重试，上游拒绝账号则 10 分钟后重试，因上游无容量中止则 30 秒后重试。停用或凭据失效的账号不补票：轮到它时会按当时的事实和参数重新确认，
续期途中被停用会在下一个请求前停手（`account_unschedulable`，不计退避，重新启用后立即恢复）。
续期途中才关闭续期开关的，本轮（最多「出口数 × 次数」个请求）仍会走完。
补票任务不加跨实例租约，每个实例各自照顾自己收到的流量。

## turn-state 模板与观测

`turn_state` crate 承载 Codex `X-Codex-Turn-State` 模板（按账号 × 模型分桶，落 `<runtime_data_dir>/turn_state/`，蓝绿实例共享）。管理接口只用 GET/POST 静态路径，票值永不返回。

| 方法 | 路径 | 请求 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/turn-state/settings` | 无 | 运行设置；`cloudMint.relayKey`、`proxyUrl` 与 `upstreamProxyUrl` 只回 `<set>`/空 |
| `POST` | `/api/admin/turn-state/settings/update` | 全部基础字段 + 可选 `cloudMint` / `warmPool` | 整体替换并热生效；`cloudMint` 省略时保留现值，`relayKey` 为 `<set>` 时沿用已保存密钥；模板/受限长度表不能重叠 |
| `GET` | `/api/admin/turn-state/observations` | 无 | 按桶的正常/受限/未知/沉默与注入盲区计数、模型对照计数（`servedMatch` / `servedMismatch` / `servedUnknown`）、48 小时分时、长度直方图、最近 100 条事件 |
| `GET` | `/api/admin/turn-state/buckets` | `account?`、`model?` | 有效模板摘要（范围、长度、签发/到期、来源、网关、命中），不含值 |
| `POST` | `/api/admin/turn-state/buckets/clear` | `{ account, model? }` | 清除模板（内存与磁盘） |

设置字段：`ttlSeconds`（30–86400，默认 240）、`injectMode`（默认 `fill-missing` 只给没带 state 的请求补上；`always` 有模板就注入，会换掉请求自带的 state；`replace-only` 只替换受限档长度的 state）、`dryRun`、`logDecisions`、`templateLengths`、`degradedLengths`（空表 = ≥200 字节可见 ASCII 下限规则）、`servedMismatchAction`（见下文「换模型」，省略时保留现值）、`cloudMint { enabled, mode, observeOnly, relayUrl, relayKey, proxyUrl, upstreamProxyUrl, gateway, ticketLen, ticketTtlSeconds, models, transport, cooldownSeconds, maxAttempts }`。
`mode=native`（默认）由 cpr 经 `upstreamProxyUrl` 指定的专用代理打票，每次尝试重新建立连接以支持动态出口 IP，缺配置或连接失败不回退到账号业务代理或直连，验收票长、`__oailb` 内嵌网关名与 `response.created` 的模型声明；`mode=relay` 交给 `deploy/cloud-mint/` 的 relay，中继到上游的出口由中继配置，`proxyUrl` 只控制 cpr 到中继的一跳；账号缺票时请求侧异步预热一次；票不定时续。后台每 20 秒检查最近 10 分钟有流量的账号：凭据里的路由 cookie 对缺失或剩余不足 120 秒时，不带既有 pair 裸打一次换一对新的。`mode=relay` 时 pair 缓存在 relay 进程里，续 pair 会带 `x-mint-fresh-pair: 1`，relay 删掉这条 pair 缓存并不带 cookie 重打；票缓存仍可复用。调用方显式带了 seed cookie 时仍用那对。

账号目录里归入「已过期」的账号（购买票据到期且已不能调度）不打票也不预热：预取、续期和手动打票统一返回「账号已过期，不打票」；票据到期但仍在正常服务的账号照常。启用专用打票后，旧的遍历业务代理自动补票任务不再同时运行。专用代理在「票据管理」页填写，读取接口与 Debug 均不回显地址或认证信息，提交 `<set>` 保留已有值，提交空字符串清除。打票不修改账号业务代理绑定，票据仍属于同一账号与模型，并随有效路由对使用。原生打票整次调用最多 24 次尝试、75 秒，单次最多 60 秒，失败后进入配置的冷却期。

云端票必须有匹配的服务模型声明和完整路由对。发布时先核对账号凭据与原路由未被并发更新，再保存路由并发布绑定该路由的票；路由写入失败不会留下新票。业务请求只使用与其凭据快照路由一致的云端票，WebSocket 发送正文前还会核对连接的实际路由。旧版本保存的未绑定路由云端票不再注入，下次缺票会重新打票；被动捕获和手动遍历的模板维持原有作用域。

`ttlSeconds` 调短对已写入的票同样生效：读取时按「签发时刻 + 当前 `ttlSeconds`」封顶（云端打票的票按 `cloudMint.ticketTtlSeconds`），调大不会延长已写入的票。

已经保存过的 `settings.json` 不会跟着代码默认值改。文件里如果还是 `injectMode: always`、`ttlSeconds: 3600`，或 `degradedLengths` 里有 `292` / `312`，行为保持旧值，直到在状态页改完保存。`degradedLengths` 不再表示降级：非空时 `always` 和 `replace-only` 仍会按这些长度换掉客户端的票，服务启动读到非空表会打一条警告。新装、或从未保存过设置的实例直接用当前默认（`ttlSeconds` 240、`fill-missing`、空的 `degradedLengths`）。

### 连接预热

`warmPool` 控制后台 WebSocket 候选，仅作用于启用 `pinTurnState` 的 OpenAI OAuth 账号

- `models` 中每个模型分别建立连接，`connectionsPerAccount` 是每账号、每模型的保留数，合计受 `maxTotalConnections` 限制；模型列表留空时跟随业务，首次使用 `gpt-6-astra`
- `probe=true` 要求完整响应、模型声明匹配且完整答案等于期望值；允许首尾空白和单层强调/行内代码，期望 `21` 不接受 `210`、`21.0` 或附带解释的答案；关闭答案检查只标记 `ready`，不标记 `verified`
- 启用专用打票且已配置代理时，候选通过该代理建立，失败后关闭连接再尝试；开启 `businessReuse` 后，候选通过才条件发布路由及新签票，业务复用同一条连接，账号保存的业务代理不改写
- `businessReuse=false` 只建立和检查候选，预热不发布账号票或路由，也不向业务提供候选连接；普通业务继续原有路径
- `probeRetries` 是首次之后的追加次数，`0` 表示每轮只尝试一次；失败槽位在 `cooldownSeconds` 后继续尝试，不影响其他已通过的槽位
- `probeTimeoutSeconds` 限制探针请求阶段，已开始的发布与失败恢复会完成；`maxAgeSeconds` 限制连接寿命，`reprobeSeconds` 控制复探，池满或其他槽位冷却不阻止已有连接复探；后台每 20 秒应用设置，配置改变会重新验证候选
- `requireVerified=true` 要求新对话使用验证通过的连接，每次账号尝试最多等待 2 秒，未就绪返回 503 和 `Retry-After: 2`；此模式须开启答案检查与业务复用，自动补票和续 pair 交由预热验证后发布；客户端自带当轮票及已有续接不改写

探针证明与连接寿命分开检查：证明期限不超过复探间隔和当前票据寿命，有签发时间时还受票据剩余时间限制。请求附带不同票据、模型不符、账号票据绑定代次或探针策略改变时，原证明不能用于该请求。复探可以更新证明，但没有新票时不改变旧票的到期上限

尚在暖池中的候选提前复探；已领用连接不插入探针，新对话遇到过期或条件变化的证明时重新选择连接。带 `previous_response_id` 的已有续接保留原协议路径，观测中如实标明探针过期或条件变化

`requireVerified` 初始为 `false`，保留无可用候选时普通建连的行为，管理更新省略该字段时保留现值；为兼容已有设置，关闭时不序列化该字段。回滚到不支持该字段的版本前，应恢复可读取的设置备份

账号详情 `warmPool` 返回已发布连接数与最近探测的模型、判定、连接 ID、网关和尝试次数。`verified` 仅表示该模型通过配置的探针，不代表所有模型或所有任务的能力保证

云端打票任务除同账号去重外，进程内最多同时执行 4 个；此限制与预热连接总上限独立

### 测智台

`POST /api/admin/accounts/test-bench` 接受 `accountId`、`modelId`、`prompt`、可选 `reasoningEffort` 和 `mode`

- `mode=business` 为默认，固定所选账号，遵守账号可用性、模型权限和并发限制，使用当前票与暖池策略
- `mode=diagnostic` 保留原始账号诊断，跳过固定票与暖池领用，可用于对照和排查停用账号
- SSE 的 `execution` 事件返回 `mode` 和 `details`，其中 `ticketAttached` 表示请求是否附带票，`warmPoolUsed` 仅表示连接来自暖池；`connectionReused`、`connectionId`、`transport` 来自实际执行观测
- `warmVerified` 表示请求发出时探针证明仍有效且条件匹配；`warmVerificationStatus` 为 `fresh`、`expired`、`conditions_changed`、`unchecked`、`pending` 或 `rejected`
- `warmVerifiedAtMs` 是最近证明的服务器 Unix 毫秒时间，`warmVerificationAgeMs` 是发出请求时由服务器计算的证明年龄，不使用浏览器时钟推断
- 未记录的事实为 `null`，旧结果没有时间信息时显示历史记录；不返回票、Cookie、认证头或证明指纹。探针通过不等于本次回答正确

业务方式仍是管理员固定账号测试，不经过 Client Key 的分组选号和限额计费；完整 API 验收使用实际 Client Key。票附带状态不等于上游已接收，传输失败仍按错误结果处理

### 换模型

每个业务响应都把上游声明的模型与实际发送的模型对照一次。上游有两处声明：响应头 `openai-model`（WebSocket 上是 metadata 帧里的同名头）和正文 `response.created` / 终态事件的 `response.model`；任一处不一致即判为换模型。比较只忽略大小写，带日期的快照名与裸名不算同一个模型；两处都没有声明记为未知，不当作一致。诊断请求和经临时出口的探测不计入。

判为换模型的这一轮，在非模拟运行下，新签发的 state 不进固定也不进会话，正在复用的那张固定按值条件失效（期间已被换成新值则不动）。这一条与 `servedMismatchAction` 无关。`servedMismatchAction` 决定额外的处置：

- `observe`（默认）：只计数，响应照常交付。
- `block`：中止本次响应并返回 `degraded_model_blocked`，不重放业务请求，不切换账号。已交付的部分不能撤回。按 pair 指纹清除本次失效路由，期间已换成另一对则保留。旧配置 `drop-pair` 可继续读取，读取后按 `block` 执行，保存时写成 `block`。
- `dryRun`：换模型只记日志和计数，不阻断、不改 pin、会话、pair 或连接，也不启动自动或手动打票。

非模拟运行的业务 WebSocket 在归还连接前自行对照模型，换模型的连接不再回池，避免消费方处理较慢时被提前复用。打票与业务请求分开：打票构造独立的 `store:false`、`stream:true` 正文，不携带业务续接 ID 或档位；业务正文保留原有 store、service_tier、previous_response_id 与 reasoning.context，共享 encoder 仍仅补缺省值。

同一账号的票仍按出口指纹分桶。实测中票可以跟着还活着的 `__cflb` / `__oailb` 换 IP，出口绑定保持原样。

对照结果和路由节点随请求记录落库，写在 `model_requests.provider_observation_json` 里：`servedMatch`（`match` / `mismatch`，未知时不写）、`upstreamGateway`（路由 cookie 对里的 `unified-N`）、`upstreamGatewaySource`（`request` 请求带出的 pair / `response` 上游这次新发的 pair / `connection` WebSocket 连接握手时用的 pair）。节点标签是路由凭据自己的声明，不是对实际执行节点的独立验证。请求日志（`/api/admin/logs/recent`）的 `servedMatch`、`gateway`、`gatewaySource` 是同一组事实。

被动捕获的 `__oailb` 按它自己 JWT 里的 `exp` 存，不按 Set-Cookie 声明的期限。

WS 保活的探针除了核对答案，还核对上游声明的模型：声明了别的模型、或没读到终态就断开的连接不算满血。
