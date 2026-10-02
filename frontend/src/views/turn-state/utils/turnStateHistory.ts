import type { UsageListRecord, UsageRecordDetail } from '@/api'

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

export function turnStateHistoryRow(record: UsageListRecord, detail: UsageRecordDetail | null, accountId: string): TurnStateHistoryRow {
  const base = {
    id: record.id,
    time: record.createdAtDisplay,
    model: record.upstreamModel ?? record.model ?? record.requestedModel ?? '—',
  }
  if (!detail || detail.accountId !== accountId) {
    return { ...base, lengths: [], result: '待核对', reason: '诊断读取失败，请刷新重试', partial: true }
  }
  const lengths = observedLengths(detail, accountId)
  // 长度仅作为响应观测，不能据此判断票是否入库或模型能力。
  const reason = detail.logicalOutcome === 'running'
    ? '请求进行中，完成后刷新查看'
    : lengths.length
      ? '已观测到上游票据，保存情况请查看当前票据列表'
      : '没有可用的票据摘要'
  return {
    ...base,
    lengths,
    result: outcomes[detail.logicalOutcome] ?? '未知结果',
    reason,
    partial: !detail.trace || detail.trace.droppedEvents > 0,
  }
}
