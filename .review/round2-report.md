## 结论

**修完上一轮 #4–#9 的残留问题，以及下列 N1–N3 再发；9 条中 3 条已闭合、6 条部分闭合。** 修复方向合理，但尚未兑现并发、取消和安全重跑的完整承诺。

审查对象为 `0e08fcf5301094a4012f69598866d08cca8122a2` 及当前 `.review/release.sh`；全程只读，未执行发版。

## 上一轮 9 条的闭合情况

| # | 状态 | 证据 | 备注 |
|---|---|---|---|
| 1 回滚兼容 | **已闭合** | `git diff --exit-code 73250d0 HEAD -- backend/crates/providers/openai/src/credential/types.rs backend/crates/providers/openai/src/credential/security.rs`：退出 **0**，输出为空 | 凭据 schema 已恢复基线，原来的旧版解码障碍消除。但新文件存储引入 N1、N2。 |
| 2 探测隔离 | **已闭合**（针对原账号状态污染） | [provider/mod.rs:601](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/provider/mod.rs:601)、[execution.rs:654](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/provider/execution.rs:654)、[failure.rs:356](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/provider/failure.rs:356) | hunt 固定走 Generate，并始终传入 `Some(DiagnosticEgress)`，包括直连。失败回写、反馈分及成功路径的 Cookie、配额、持久化亲和均受闸门控制；不是所有传输缓存都隔离，见后文。 |
| 3 pin 并发 | **已闭合** | [turn_state_pin.rs:183](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/turn_state_pin.rs:183)、[turn_state_pin.rs:277](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/turn_state_pin.rs:277) | 两处使用同一个 `self.0` Mutex。hunt 先写时，旧请求被锁内检查挡住；被动捕获先写时，hunt 在同一临界区删除客户端 pin 后写账号级 pin。反向交错也成立。 |
| 4 绑定校验 | **部分闭合** | [turn_state_hunt.rs:477](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/turn_state_hunt.rs:477)、[turn_state_hunt.rs:523](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/turn_state_hunt.rs:523)、[admin.rs:683](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/admin.rs:683) | 能发现校验前的代理修改和回读前的绑定不一致；回读之后仍可并发改绑，pin 提交只核对凭据，不核对出口。 |
| 5 取消边界 | **部分闭合** | [turn_state_hunt.rs:130](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/turn_state_hunt.rs:130)、[turn_state_hunt.rs:455](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/turn_state_hunt.rs:455)、[前端:136](/home/wuxiran/worktrees/cpr-review-hunt-20260920/frontend/src/views/accounts/composables/useAccountTurnStateHunt.ts:136) | 接收端已关闭的情况修好了；`hit` 入缓冲后、浏览器收到前取消，仍会继续绑定，而界面承诺不会改动账号。 |
| 6 失败分类 | **部分闭合** | [FailureClass:645](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/turn_state_hunt.rs:645)、[observation.rs:775](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/provider/observation.rs:775) | 认证、权限、额度、模型不支持已使用真实 Provider 分类；`InvalidRequest` 中止遍历合理。但 `Some(_) => Egress` 仍把本地基础设施错误和模型容量拒绝当成换出口依据。 |
| 7 续期资格 | **部分闭合** | [turn_state_renewal.rs:77](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/turn_state_renewal.rs:77)、[turn_state_hunt.rs:260](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/turn_state_hunt.rs:260)、[turn_state_hunt.rs:322](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/turn_state_hunt.rs:322) | 排队期间停用、关闭续期已有重查；执行中只在每个出口前检查 enabled/credential，既不逐请求重查，也不检查续期意图。重取列表后还继续使用旧参数。 |
| 8 脚本失败退出 | **部分闭合** | [release.sh:71](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/release.sh:71)、[release.sh:95](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/release.sh:95)、[release.sh:111](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/release.sh:111) | 末尾健康检查现已决定退出状态；但两个 `if` 内的状态查询失败仍被吞掉。只读复现四个案例均继续执行、退出 **0**。 |
| 9 幂等与恢复 | **部分闭合** | [release.sh:13](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/release.sh:13)、[rollout.py:81](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/rollout.py:81)、[rollout.py:274](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/rollout.py:274) | 正常完成后重跑可以避免覆盖基线槽位；但磁盘 active 不等于 gate 实际加载的 active，且“第 3 步失败必定保持旧入口”的恢复说明不成立。 |

以下是仍需修复的具体边界。

- **#4：绑定与 pin 仍未形成一致提交。** 回读确认出口 A 后，另一个管理请求可改成 B；随后 [`turn_state_hunt_pin()`](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/admin.rs:683) 只比较凭据 ticket，仍能钉 A 的 state 并报告成功。`already_bound` 路径同样存在。应把探测出口身份/版本带进条件提交，并让 pin 的有效作用域包含出口版本，或让所有改绑路径统一使旧 pin 失效；单纯再加一次回读仍有窗口。

  “同地址不同 ID”不是当前正常写入路径下已证明的缺陷：代理创建会拒绝已有 URL，[更新也检查重复 URL](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-store/src/postgres/proxies.rs:580)。历史库是否存在重复记录尚未核实。

  此外修正上一轮判断：[`publish_committed()`](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/mod.rs:95) 不传播实际发布失败；[发布器忽略刷新错误](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-core/src/runtime/mod.rs:218)，但[配置无法确认时暂停新请求](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-core/src/runtime/mod.rs:194)。因此不能把“提交成功、发布失败”直接描述成此次会继续用旧出口发送新请求；**可以确定的是回读不能证明发布成功，也不能挡住并发改绑。**

- **#5：入队不能兑现“送达前取消不改账号”。** 容量 64 的通道足以缓存 `hit`；即使 SSE 层已取走事件，网络仍可能尚未交给浏览器。前端只有[处理 `hit` 后才禁止取消](/home/wuxiran/worktrees/cpr-review-hunt-20260920/frontend/src/views/accounts/composables/useAccountTurnStateHunt.ts:78)。因此当前承诺仍被违反。更稳的做法是服务端维护任务状态，让取消与提交原子竞争，由取消接口返回“已取消”或“已进入提交”；只有前者显示账号未改动。再检查一次 `is_closed()` 或缩小缓冲不能解决浏览器送达问题。

- **#6：需明确“中止本轮”的非出口类别。** 本地凭据、Store、Coordinator 错误真实映射为 [`ProviderInfrastructureUnavailable`](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/provider/observation.rs:775)，却会继续枚举代理。模型容量拒绝也明确映射为 [`UpstreamCapacityUnavailable`](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/provider/failure.rs:1316)，现有传输层已经为它提供[有界同账号重试](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/tests/provider/contract.rs:5635)，hunt 又追加逐出口尝试。应区分账号拒绝、系统/请求中止、出口可重试，避免兜底全部换出口。`InvalidRequest` 放入“停止遍历”合理，但不代表凭据失效；`Protocol` 可能来自出口返回错误协议，不能一概视为账号问题。Continuation/Cancelled 的实际可达性见验证缺口。

- **#7：最新列表只检查 ID，丢弃了最新配置。** 管理员在排队期间改模型、次数或关闭直连，账号仍在到期列表时，worker 会继续使用[周期开始时的旧参数](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/turn_state_renewal.rs:82)。同一出口第一次请求期间停用账号，后续最多剩余 19 次请求仍可继续；若该次命中，还可进入收尾。关闭续期更不会被 `still_schedulable()` 发现。应使用重新读取的参数，并在每次请求及提交前核对自动续期意图和资格。

- **#8：查询失败仍会继续部署。** 我提取原 `status_fact` 函数，用 `false` 和非法 JSON 分别模拟查询失败、解析失败，对 `active-image`、`legacy` 两种条件执行只读测试；四次都输出 `skipped`、`continued-to-deploy`，退出 **0**。原因是 `die` 退出的是命令替换子 shell，外层 `if` 把失败当成条件不成立。应先独立赋值并检查退出码，再比较值。[nginx 的 `grep` 条件](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/release.sh:78) 也没有区分“不匹配”和读取错误，更不能仅凭没找到 18082 就断言已指向 18083。

- **#9：恢复说明需按真实阶段重写。**
  - [第 1 步 reload](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/release.sh:83) 失败会直接退出，没有还原，故“第 1 步失败已自动还原”不成立。
  - rollout 先[写 upstream 文件再 reload](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/rollout.py:274)。若在两者之间中断，`status` 已认为目标槽位 active，但 gate 仍可能服务旧槽位；两个 `/healthz` 都能是 204，重跑便错误宣布目标已生效。幂等检查需确认实际流量所达版本，并能恢复未完成的切流。
  - [切流后排空及记录文件写入](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/rollout.py:302) 已在恢复 `try/except` 外。此处失败会非零退出，但入口已经切到新版，不一定有 `aborted`，不能照脚本提示认定仍在旧槽位。
  - [`rollback`](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/rollout.py:343) 要求另一槽位存在、非 legacy 且已停止；立即回滚可能因仍在排空而拒绝。旧镜像 `deploy` 也要求[当前槽位运行、目标槽位停止](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/rollout.py:220)，不是所有失败状态下都能直接执行的兜底命令。

## 新发现：必须修

**N1．文件存储既有丢更新风险，也有破坏文件内容的并发窗口。**

证据：[读取错误转空表](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/turn_state_auto_hunt.rs:41)、[无锁读改写及固定临时文件名](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/turn_state_auto_hunt.rs:60)。

两个实例读取同一旧表后分别修改不同账号，后写者会丢掉前者修改；这也可能把已经关闭的续期重新带回来。更严重的是，两者同时打开 `auto_hunt.tmp` 会截断、写入同一个 inode；一方 rename 后，另一方仍可能通过已打开的描述符改写正式文件，之后自身 rename 却报失败。

损坏或读取失败又被当成空表：续期静默停止，下一次成功保存可能覆盖其他账号全部配置。纯写入只读故障通常会返回错误；但读取失败后执行删除可因“没有变化”返回成功。目录尚不存在且磁盘只读时，[初始化会失败并阻止 Provider 启动](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/lib.rs:222)。

应锁住完整跨进程读改写过程，使用唯一临时文件，并区分文件不存在、读取失败和内容损坏；错误时禁止基于空表覆盖。

**N2．`prepare_rotation` 提前落盘，使失败的管理操作仍然生效。**

证据：[准备阶段写配置](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/admin.rs:1214)，随后才[校验并执行凭据提交](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/openai.rs:219)。

对于已经开启 pin 的账号，开启续期的文件写入成功后，即使凭据 CAS 冲突、数据库提交失败，后台仍能读取配置并开始真实探测。关闭 pin 的操作也可能先删除续期配置，再因提交失败保持 pin 开启。现有失败路径没有撤销该文件副作用。

这不是可接受的“prepare”行为。续期意图应在管理提交确认后才生效，并明确跨存储失败的恢复方式；不能让 API 报失败而后台配置已改变。

**N3．脚本校验的镜像包不一定是实际部署的镜像包。**

证据：[release.sh:31](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/release.sh:31)、[release.sh:69](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/release.sh:69)、[release.sh:103](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/release.sh:103)；rollout 从[metadata 选择镜像](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/rollout.py:208)，并[加载 metadata 指定的 archive](/home/wuxiran/worktrees/cpr-review-hunt-20260920/.review/rollout.py:252)。

若固定文件名的 metadata 遗留为另一构建，脚本可以通过当前 `$ARCHIVE` 校验，却部署 metadata 指向的其他版本；直到切流、开始排空后才发现 `$IMAGE` 不符。

应在退役 legacy 前断言 metadata 的镜像、包路径、包摘要与本次目标一致，并使用同一份校验结果部署。

## 新发现：建议修

- **续期重查改成按账号读取。** 每个到期账号重新枚举全体账号，内部还逐个加载凭据，约为 `O(到期账号数 × 账号数)`；应直接取该账号的最新意图和资格。[worker:77](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/turn_state_renewal.rs:77)、[Provider:613](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/admin.rs:613)。

- **`account_unschedulable` 不宜无条件继承 15 分钟拒绝退避。** 管理员很快重新启用或修复凭据后，仍会被旧退避挡住；这是本地状态变化，不是上游限流证据。可清理该状态的退避，或使退避随资格版本变化失效。[turn_state_renewal.rs:112](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/turn_state_renewal.rs:112)。

- **区分账号事实隔离与传输缓存共享。** 成功 Cookie、配额、持久化亲和确实受控，但 [WebSocket 成功恢复状态](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/provider/execution.rs:1014) 和连接池仍可更新。已核实[池 key 包含出口](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/transport/client_sse.rs:490)、[breaker key 包含账号出口指纹](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/transport/client.rs:698)，未发现跨不同出口复用连接的证据；建议补成功路径测试，避免把现状描述成完全无共享状态。

- **前端请求计数仍与后端不同。** 每个 `attempt` 都加一，失败终止时没有最终计数纠正，因此 `NotSent` 仍会显示为已发请求。[前端:61](/home/wuxiran/worktrees/cpr-review-hunt-20260920/frontend/src/views/accounts/composables/useAccountTurnStateHunt.ts:61)、[后端:382](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/src/use_case/turn_state_hunt.rs:382)。

## 没能核实的

- **没有执行 Rust、浏览器或生产集成测试。** 为遵守不修改文件，未运行生成构建产物的命令。新增[取消测试](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-admin/tests/use_case/turn_state_hunt.rs:583)覆盖的是“探测返回前接收端已销毁”，未覆盖 `hit` 已入队但浏览器未收到；新增[隔离测试](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/tests/provider/contract.rs:6793)主要覆盖 401 凭据状态，不能代替成功配额、Cookie、WS 的运行验证。

- **Continuation/Cancelled 在此 hunt 探针中的真实触发路径未确认。** 分类兜底确实会将它们归入 Egress，但探针使用[无续接信息的固定请求](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/admin.rs:1808)和[独立新建的 CancellationToken](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-core/src/engine/execution.rs:828)，不据此声称已经发生这两类错误的重复发送。

- **revive 自由文本日志仍不能证明完全脱敏。** [过滤器](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/providers/openai/src/credential/revive/client.rs:293)仅识别 `eyJ` 或连续至少 40 字符的凭据样字符串；短密码、带分隔符的敏感内容不在证明范围内。没有上游真实错误样本，不能认定已泄露，也不能认定该风险已闭合。

- **来源校验和生产恢复前提未实测。** [缺少 `Sec-Fetch-Site` 时仍放行](/home/wuxiran/worktrees/cpr-review-hunt-20260920/backend/crates/gateway-api/src/admin/accounts/handlers.rs:612)，不能仅凭缺头断言请求来自非浏览器。旧镜像、基线迁移清单是否确实保留，及 legacy 排空期间调用方 DNS/连接行为，也没有生产证据。

只读检查结果：`bash -n .review/release.sh`、`git diff --check 9466049 HEAD` 均退出 **0**，`rollout.py` AST 解析成功；结束时 HEAD 未变，`git status --short` 仍只有原来的 `?? .review/`。这些结果不代表运行验收通过。