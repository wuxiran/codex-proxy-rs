/** 观澜 CDK 兑换 SDK。浏览器同源走 `/guanlan`，由 Vite / Nginx 反代到 zzledu。 */

const CLIENT_STORAGE_KEY = 'cpr.guanlan.cdk-client'
const CLIENT_VERSION = '20260907-receipt-capacity'
const GUANLAN_BASE = '/guanlan'
const MAX_CDK_COUNT = 200
const CDK_PATTERN = /^CDK(?:-[A-Z0-9]{4}){8}$/i

export class GuanlanCdkError extends Error {
  constructor(
    message: string,
    readonly network = false,
  ) {
    super(message)
    this.name = 'GuanlanCdkError'
  }
}

export function parseGuanlanCdkCodes(value: string) {
  const codes = value
    .split(/[\s,;]+/)
    .map(code => code.trim().toUpperCase())
    .filter(Boolean)

  if (codes.length === 0)
    throw new GuanlanCdkError('请至少粘贴一个观澜 CDK')
  if (codes.length > MAX_CDK_COUNT)
    throw new GuanlanCdkError(`单次最多兑换 ${MAX_CDK_COUNT} 个 CDK`)
  if (codes.some(code => !CDK_PATTERN.test(code)))
    throw new GuanlanCdkError('CDK 格式不正确，应为 CDK-XXXX-XXXX-... 共 8 段')

  return [...new Set(codes)]
}

export async function redeemGuanlanCdks(cdks: string[]): Promise<Record<string, unknown>> {
  const client = await loadOrCreateGuanlanClientId()
  const ticket = await redeemTicket(cdks, client, false).catch(async (error) => {
    if (error instanceof GuanlanCdkError && error.message.includes('已兑换'))
      return redeemTicket(cdks, client, true)
    throw error
  })
  return downloadExport(ticket, client)
}

async function redeemTicket(cdks: string[], client: string, recover: boolean) {
  const payload: Record<string, unknown> = { cdks }
  if (recover)
    payload.download = true

  const data = await guanlanRequest<RedeemResponse>('/api/cdk/redeem', {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'idempotency-key': crypto.randomUUID(),
      'x-cdk-client': client,
      'x-cdk-client-version': CLIENT_VERSION,
    },
    body: JSON.stringify(payload),
  })

  if (!data.ok)
    throw new GuanlanCdkError(publicRedeemError(data))

  const download = data.downloads?.[0]
  const redemptionId = data.redemption_id || download?.redemption_id
  const downloadToken = data.download_token || download?.download_token
  if (!redemptionId || !downloadToken || !['redeemed', 'recovered'].includes(data.status || ''))
    throw new GuanlanCdkError('观澜兑换结果不完整，请稍后重试')

  return { redemptionId, downloadToken }
}

async function downloadExport(
  ticket: { redemptionId: string, downloadToken: string },
  client: string,
) {
  const data = await guanlanRequest<Record<string, unknown>>(
    `/api/cdk/redemptions/${ticket.redemptionId}/download`,
    {
      method: 'GET',
      headers: {
        'x-cdk-client': client,
        'x-cdk-client-version': CLIENT_VERSION,
        'x-cdk-download-token': ticket.downloadToken,
      },
    },
    true,
  )
  if ('guanlan_signed_export' in data)
    return data
  if (!Array.isArray(data.accounts) || data.accounts.length === 0)
    throw new GuanlanCdkError('观澜未返回可导入的账号文件')
  return data
}

async function guanlanRequest<T>(path: string, init: RequestInit, preserveSigned = false): Promise<T> {
  let response: Response
  try {
    response = await fetch(`${GUANLAN_BASE}${path}`, init)
  }
  catch {
    throw new GuanlanCdkError('无法连接观澜兑换服务', true)
  }

  let data: unknown = null
  const raw = await response.text()
  try {
    data = JSON.parse(raw)
  }
  catch {
    data = null
  }

  if (!response.ok) {
    const payload = isRecord(data) ? data : {}
    if (response.status === 429)
      throw new GuanlanCdkError('观澜兑换被限流，请稍后重试')
    throw new GuanlanCdkError(
      publicRedeemError({
        error: typeof payload.error === 'string' ? payload.error : undefined,
        error_code: typeof payload.error_code === 'string' ? payload.error_code : undefined,
      }),
      response.status >= 500,
    )
  }

  if (preserveSigned) {
    if (!isRecord(data) || !Array.isArray(data.accounts) || data.accounts.length === 0 || !isRecord(data.x_revive_manifest))
      throw new GuanlanCdkError('观澜未返回有效的原始签名文件')
    return { guanlan_signed_export: raw } as T
  }
  return data as T
}

async function loadOrCreateGuanlanClientId() {
  const existing = window.localStorage.getItem(CLIENT_STORAGE_KEY)
  if (existing && /^r1-[a-f0-9]{64}$/i.test(existing))
    return existing

  const seed = crypto.getRandomValues(new Uint8Array(32))
  const digest = await crypto.subtle.digest('SHA-256', seed)
  const client = `r1-${[...new Uint8Array(digest)].map(byte => byte.toString(16).padStart(2, '0')).join('')}`
  window.localStorage.setItem(CLIENT_STORAGE_KEY, client)
  return client
}

function publicRedeemError(payload: { error?: string, error_code?: string }) {
  const code = typeof payload.error_code === 'string' ? payload.error_code : ''
  if (code === 'invalid_format')
    return 'CDK 格式不正确，请检查卡密'
  if (code === 'not_found')
    return 'CDK 不存在或无效'
  if (code === 'already_redeemed')
    return 'CDK 已兑换且无法再次取回，请改用已下载的 JSON 导入'
  const error = typeof payload.error === 'string' ? payload.error : ''
  if (error && !/cdk-|token/i.test(error))
    return error
  return '观澜拒绝了 CDK 兑换'
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

interface RedeemResponse {
  ok?: boolean
  status?: string
  redemption_id?: string
  download_token?: string
  error?: string
  error_code?: string
  downloads?: Array<{
    redemption_id?: string
    download_token?: string
  }>
}
