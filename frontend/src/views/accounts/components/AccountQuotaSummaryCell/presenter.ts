import type { AccountQuotaWindow, AccountQuotaWindowEntry, AccountRow } from '../../constants'
import { formatUsd } from '@/views/usage/utils/format'
import { quotaWindowLocalUsd, readUsdCost } from '../AccountUsageWindow/presenter'

type AccountModelUsage = AccountRow['usage']['models'][number]

export interface AccountQuotaUsdSummary {
  used: number
  usedDisplay: string
  estimatedQuota: number | null
  estimatedQuotaDisplay: string | null
  title: string
}

/**
 * 根据账号最近一次模型请求选择对应额度组。
 * 没有专属额度的模型统一回退到 Codex 通用额度。
 */
export function recentlyUsedQuotaEntry(
  entries: readonly AccountQuotaWindowEntry[],
  models: readonly AccountModelUsage[],
) {
  const recentModel = latestModelUsage(models)
  if (recentModel) {
    const modelKey = quotaIdentity(recentModel.model)
    const modelEntry = entries.find(entry => quotaEntryIdentities(entry).has(modelKey))
    if (modelEntry)
      return modelEntry
  }

  return entries.find(entry => entry.windows.some(window => window.limitId === 'codex'))
    ?? entries[0]
}

/** 一个额度组可能包含多个滚动窗口，摘要使用其中当前占用最高的窗口。 */
export function representativeQuotaWindow(entry: AccountQuotaWindowEntry | undefined) {
  return entry?.windows.reduce<AccountQuotaWindow | undefined>((representative, window) => {
    if (typeof window.usedPercent !== 'number')
      return representative
    if (
      !representative
      || typeof representative.usedPercent !== 'number'
      || window.usedPercent > representative.usedPercent
    ) {
      return window
    }
    return representative
  }, undefined)
}

function latestModelUsage(models: readonly AccountModelUsage[]) {
  let latest: AccountModelUsage | undefined
  let latestTimestamp = Number.NEGATIVE_INFINITY

  for (const model of models) {
    const timestamp = Date.parse(model.lastUsedAt)
    if (!Number.isFinite(timestamp) || timestamp <= latestTimestamp)
      continue
    latest = model
    latestTimestamp = timestamp
  }

  return latest
}

function quotaEntryIdentities(entry: AccountQuotaWindowEntry) {
  const identities = new Set<string>()
  addQuotaIdentity(identities, entry.label)
  for (const window of entry.windows) {
    addQuotaIdentity(identities, window.limitId)
    addQuotaIdentity(identities, window.limitName)
  }
  return identities
}

function addQuotaIdentity(identities: Set<string>, value: string | null) {
  const identity = quotaIdentity(value)
  if (identity)
    identities.add(identity)
}

function quotaIdentity(value: string | null) {
  return value?.trim().toLowerCase().replace(/[^a-z0-9]+/g, '') ?? ''
}

/**
 * 列表用量列的美元摘要。已用金额来自本机记录；额度按官方 usedPercent 反推，
 * 算法与 Sub2API 账号行 `cost × 100 / utilization` 相同，不是上游账单。
 */
export function accountQuotaUsdSummary(
  account: AccountRow,
  window: AccountQuotaWindow | undefined,
): AccountQuotaUsdSummary | null {
  const used = quotaWindowLocalUsd(window) ?? readUsdCost(account.usage.costs)
  if (!used || used.amount <= 0)
    return null

  const percent = window?.usedPercent
  const estimatedQuota = estimatedQuotaUsd(used.amount, percent)
  const estimatedQuotaDisplay = estimatedQuota == null ? null : formatUsd(estimatedQuota)
  const percentDisplay = window?.usedPercentDisplay && window.usedPercentDisplay !== '—'
    ? window.usedPercentDisplay
    : null
  const title = estimatedQuotaDisplay && percentDisplay
    ? `本机已用 ${used.display}，按官方使用率 ${percentDisplay} 推算额度约 ${estimatedQuotaDisplay}，不含站外消耗`
    : `本机已用 ${used.display}，不含站外消耗`

  return {
    used: used.amount,
    usedDisplay: used.display,
    estimatedQuota,
    estimatedQuotaDisplay,
    title,
  }
}

function estimatedQuotaUsd(used: number, usedPercent: number | null | undefined) {
  if (
    used <= 0
    || typeof usedPercent !== 'number'
    || !Number.isFinite(usedPercent)
    || usedPercent <= 0
  ) {
    return null
  }

  const estimate = (used * 100) / usedPercent
  return Number.isFinite(estimate) && estimate > 0 ? estimate : null
}
