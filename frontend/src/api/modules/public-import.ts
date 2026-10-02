import type { RequestOptions } from '../request'
import request from '../request'

export interface PublicImportConfig {
  enabled: boolean
  token: string
  groupIds: string[]
  pinTurnState: boolean
  // RFC 3339；null 表示长期有效
  expiresAt: string | null
  updatedAt: string
}

export type PublicImportConfigUpdate = Pick<PublicImportConfig, 'enabled' | 'groupIds' | 'pinTurnState' | 'expiresAt'>

export function getPublicImportConfig(options: RequestOptions = {}) {
  return request<PublicImportConfig>({
    url: '/api/admin/public-import',
    method: 'GET',
    ...options,
  })
}

export function updatePublicImportConfig(data: PublicImportConfigUpdate) {
  return request<PublicImportConfig>({
    url: '/api/admin/public-import/update',
    method: 'POST',
    data,
  })
}

export function rotatePublicImportToken() {
  return request<PublicImportConfig>({
    url: '/api/admin/public-import/rotate-token',
    method: 'POST',
    data: {},
  })
}

export interface PublicImportEntry {
  groupNames: string[]
  pinTurnState: boolean
  expiresAt: string | null
  maxAccounts: number
}

export interface PublicImportItem {
  index: number
  name: string | null
  status: 'imported' | 'failed'
  importedAccounts: number
  proxyName: string | null
  statePinned: boolean
  message: string | null
}

export interface PublicImportResult {
  total: number
  imported: number
  failed: number
  items: PublicImportItem[]
}

// 密链页面没有会话，令牌只放在请求头里，不进入 URL 查询串和访问日志。
const TOKEN_HEADER = 'x-import-token'

export function getPublicImportEntry(token: string, options: RequestOptions = {}) {
  return request<PublicImportEntry>({
    url: '/api/public-import/entry',
    method: 'GET',
    headers: { [TOKEN_HEADER]: token },
    ...options,
  })
}

export function submitPublicImport(token: string, data: Record<string, unknown>, options: RequestOptions = {}) {
  return request<PublicImportResult>({
    url: '/api/public-import/accounts',
    method: 'POST',
    headers: { [TOKEN_HEADER]: token },
    data: { data },
    ...options,
  })
}

export interface PublicTicketImport {
  /** 每行 `邮箱----密码----2FA密钥`。 */
  tickets: string[]
  purchaseAmount: string
  purchaseCurrency: 'CNY' | 'USD'
  /** RFC 3339。 */
  expiresAt: string
}

/** 票据导入：服务端逐个随机出口登录建号，并加密保存票据、买入价与到期时间。 */
export function submitPublicTickets(token: string, data: PublicTicketImport, options: RequestOptions = {}) {
  return request<PublicImportResult>({
    url: '/api/public-import/tickets',
    method: 'POST',
    headers: { [TOKEN_HEADER]: token },
    data,
    ...options,
  })
}
