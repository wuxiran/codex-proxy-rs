# Excel / Basispoints (BPS) 上游协议

BPS 是 ChatGPT Excel 插件网关 `https://bps.openai.com/basispoints/api/responses`，
用账号现成的 ChatGPT OAuth access token 直接打，绕开 Codex 后端节点路由。上游唯一的
工具是 `run_officejs`，客户端工具被塞进它的信封再解析回来。

本模块只做协议转换（纯函数）。传输、账号选择、计费、错误归属由 provider 层负责。

## 来源

移植自 hloolx/codex2api（其 README 声明 MIT）。原始 Basispoints 改动作者：hloolx。
参照 ranxi2001/sub2api 的 Go 实现（`backend/internal/service/basispoints/*`）逐条比对；
另参考 JaxsonWang/cpa-plugin-oai-basispoints 的工具目录/信封/回放实现。
只移植协议本身，账号设置、OAuth 凭据生命周期、代理传输、计费与前端均为 cpr 自有。

## 已知限制

- 无 `previous_response_id` / `item_reference`；每回合需完整历史。
- `tool_choice` 只支持 auto / none；hosted 工具静默丢弃并在提示里告知。
- effort 封顶 xhigh；none/minimal→low。
- 工具项在 `response.completed` 之后一次性下发；文本增量实时。
- 一次一个工具，`parallel_tool_calls` 恒 false。
- 内容只支持文本与 https 图片 URL；`data:`、`file_id`、音频等拒绝。
- 重放缓存进程内有界（1024 条 / 16 MiB / 单条 1 MiB）。
- 与 Codex 共享配额；上游 401 不刷新令牌。
- 可用模型少，上游按 Excel 产品 agent profile 运行，行为与原生 Codex 不完全一致。
