import type { TurnStateCaptureRule, UsageListRecord, UsageRecordDetail } from '@/api'

// diagnostics/capture.rs 对非白名单头名使用 SHA-256 字段名，只读取长度摘要。
const stateHeaderKey = 'field_32d2463cbff63dc265904a36bcfdafdfc999f6914640e0af9bcc15d663361d71'

export interface TurnStateHistoryRow {
  id: string
  time: string
  model: string
  lengths: number[]
  result: string
  reason: string
  partial: boolean
  matchesRule: boolean
}

function object(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined
}

function fingerprintLengths(value: unknown): number[] {
  if (Array.isArray(value))
    return value.slice(0, 64).flatMap(fingerprintLengths)
  const item = object(value)
  if (!item)
    return []
  if (typeof item.bytes === 'number' && Number.isSafeInteger(item.bytes) && item.bytes >= 0)
    return [item.bytes]
  return Array.isArray(item.sample) ? item.sample.slice(0, 4).flatMap(fingerprintLengths) : []
}

function headerLengths(value: unknown): number[] {
  const headers = object(value)
  if (!headers)
    return []
  return Object.entries(headers).flatMap(([name, fingerprint]) =>
    name.toLowerCase() === 'x-codex-turn-state' || name === stateHeaderKey
      ? fingerprintLengths(fingerprint)
      : [])
}

function observedLengths(detail: UsageRecordDetail, accountId: string): number[] {
  const events = detail.trace?.events ?? []
  const owners = new Map<number, string>()
  for (const event of events) {
    if (event.stage === 'account.selected' && typeof event.data.accountId === 'string')
      owners.set(event.attemptIndex, event.data.accountId)
  }
  const lengths = events.flatMap((event) => {
    // 同一请求可能换号；不能把其他账号或客户端提交的 state 算到本账号。
    if (owners.get(event.attemptIndex) !== accountId)
      return []
    if (event.stage === 'upstream.response.headers' || event.stage === 'upstream.connection')
      return headerLengths(event.data.headers)
    if (event.stage === 'upstream.event' && ['response.metadata', 'codex.response.metadata'].includes(String(event.data.eventType)))
      return headerLengths(object(event.data.metadata)?.headers)
    return []
  })
  return [...new Set(lengths)]
}

const outcomes: Record<string, string> = {
  succeeded: '完整成功',
  incomplete: '未完整成功',
  failed: '失败',
  cancelled: '已取消',
  running: '进行中',
}

export function turnStateHistoryRow(record: UsageListRecord, detail: UsageRecordDetail | null, accountId: string, rule: TurnStateCaptureRule | null): TurnStateHistoryRow {
  const base = {
    id: record.id,
    time: record.createdAtDisplay,
    model: record.upstreamModel ?? record.model ?? record.requestedModel ?? '—',
  }
  if (!detail || detail.accountId !== accountId) {
    return { ...base, lengths: [], result: '待核对', reason: '诊断读取失败，请刷新重试', partial: true, matchesRule: false }
  }
  const lengths = observedLengths(detail, accountId)
  const expectedLength = rule?.modelLengths[base.model] ?? rule?.defaultLength
  const matchesRule = expectedLength != null && lengths.includes(expectedLength)
  let reason: string
  if (detail.route === '/api/admin/accounts/connection-test' || detail.protocol === 'admin_connection_test')
    reason = '连接测试不参与固定'
  else if (detail.requestKind === 'prewarm')
    reason = '预热请求不参与固定'
  else if (detail.route !== '/v1/responses')
    reason = '该接口不参与固定'
  else if (detail.logicalOutcome === 'running')
    reason = '请求进行中，完成后刷新查看'
  else if (!lengths.length)
    reason = '没有可用的 state 摘要，无法判断是否返回'
  else if (!rule)
    reason = '当前捕获规则未就绪，无法判断'
  else if (expectedLength == null)
    reason = '当前套餐未配置此模型的捕获规则'
  else if (!matchesRule)
    reason = detail.logicalOutcome === 'succeeded' ? `未满足 ${expectedLength} 字节筛选` : `未满足 ${expectedLength} 字节筛选；请求未完整成功`
  else if (detail.logicalOutcome !== 'succeeded')
    reason = `收到 ${expectedLength} 字节，但请求未完整成功`
  else
    reason = `符合 ${expectedLength} 字节候选条件；是否固定以当前状态为准`
  return {
    ...base,
    lengths,
    matchesRule,
    result: outcomes[detail.logicalOutcome] ?? '未知结果',
    reason,
    partial: !detail.trace || detail.trace.droppedEvents > 0,
  }
}
