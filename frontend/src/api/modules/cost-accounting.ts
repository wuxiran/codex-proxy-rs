import type { RequestOptions } from '../request'
import request from '../request'

/** 价格不区分币种，按管理员口径与美元 1:1 对比。 */
export interface AccountPurchase {
  accountId: string
  name: string
  email: string | null
  price: number | null
  purchasedAt: string
  retiredAt: string | null
  note: string | null
}

export interface AccountCostRow extends AccountPurchase {
  /** 账号已删除时为 false，其余现状字段为空。 */
  accountExists: boolean
  enabled: boolean | null
  credentialReady: boolean | null
  quotaExhausted: boolean | null
  planType: string | null
  usageUsd: number
  requestCount: number
  totalTokens: number
  costPerUsd: number | null
}

export interface CostDay {
  day: string
  purchasedCount: number
  spend: number
  usageUsd: number
  requestCount: number
  totalTokens: number
  costPerUsd: number | null
  cumulativeCostPerUsd: number | null
}

export interface CostTotals {
  purchasedCount: number
  spend: number
  usageUsd: number
  requestCount: number
  totalTokens: number
  costPerUsd: number | null
}

interface DateRange {
  from: string
  to: string
}

export function getCostDaily(data: DateRange, options: RequestOptions = {}) {
  return request<{ days: CostDay[], totals: CostTotals }>({
    url: '/api/admin/cost-accounting/daily',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getCostAccounts(data: DateRange & { includeRetired?: boolean }, options: RequestOptions = {}) {
  return request<{ items: AccountCostRow[] }>({
    url: '/api/admin/cost-accounting/accounts',
    method: 'GET',
    params: data,
    ...options,
  })
}

/** 省略的字段保持原值；`price: null` / `note: null` 才会清除。 */
export function setAccountPurchase(data: { accountId: string, price?: number | null, purchasedAt?: string, note?: string | null }) {
  return request<AccountPurchase>({
    url: '/api/admin/cost-accounting/purchase',
    method: 'POST',
    data,
  })
}

/** 下线只是标记：不停用账号，也不影响调度。 */
export function setAccountsRetired(data: { accountIds: string[], retired: boolean }) {
  return request<{ accountIds: string[] }>({
    url: '/api/admin/cost-accounting/retire',
    method: 'POST',
    data,
  })
}
