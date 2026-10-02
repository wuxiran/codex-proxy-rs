/** 观澜 CDK 兑换 SDK。浏览器同源走 `/guanlan`，由 Vite / Nginx 反代到 zzledu。 */

const CLIENT_STORAGE_KEY = 'cpr.guanlan.cdk-client'
/** 与观澜兑换页 `/cdk/` 的协议版本一致（2026-09-22：多空间下载 + 接收回执 + format 参数）。 */
export const GUANLAN_CDK_CLIENT_VERSION = '20260922-credentials'
const CLIENT_VERSION = GUANLAN_CDK_CLIENT_VERSION
const GUANLAN_BASE = '/guanlan'
/** 已签名 JSON（导入与 401 复活都依赖它）；`cpa` 是 ZIP，不用。 */
const DOWNLOAD_FORMAT = 'sub2api'
const REDEMPTION_ID_PATTERN = /^[\w-]{1,128}$/
const CLIENT_VERSION_PATTERN = /^[\w.-]{1,100}$/
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

/**
 * 兑换（或找回）CDK，返回已签名账号文件。一批 CDK 可能分属多个空间，
 * 每个空间一份独立签名的文件，按观澜返回顺序全部下载，不能合并（合并会破坏签名）。
 */
export async function redeemGuanlanCdks(cdks: string[]): Promise<Record<string, unknown>[]> {
  const client = await loadOrCreateGuanlanClientId()
  const ticket = await redeemTicket(cdks, client, false).catch(async (error) => {
    if (error instanceof GuanlanCdkError && error.message.includes('已兑换'))
      return redeemTicket(cdks, client, true)
    throw error
  })
  const documents: Record<string, unknown>[] = []
  for (const download of ticket.downloads) {
    documents.push(await downloadExport(download, client))
    // 接收回执只告诉观澜「文件已送达」，失败不影响导入（观澜下次对账会补齐）。
    await acknowledgeReceipt(download, client, ticket.idempotencyKey)
  }
  return documents
}

interface DownloadTicket {
  redemptionId: string
  downloadToken: string
}

async function redeemTicket(cdks: string[], client: string, recover: boolean) {
  const payload: Record<string, unknown> = { cdks }
  if (recover)
    payload.download = true

  const idempotencyKey = crypto.randomUUID()
  const data = await guanlanRequest<RedeemResponse>('/api/cdk/redeem', {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'idempotency-key': idempotencyKey,
      'x-cdk-client': client,
      'x-cdk-client-version': CLIENT_VERSION,
    },
    body: JSON.stringify(payload),
  })

  if (!data.ok)
    throw new GuanlanCdkError(publicRedeemError(data))

  // 与观澜兑换页一致：优先 `downloads`（多空间），没有时退回顶层单文件字段。
  const downloads = (data.downloads ?? [])
    .map(item => downloadTicket(item.redemption_id, item.download_token))
    .filter((item): item is DownloadTicket => item !== null)
  if (downloads.length === 0) {
    const single = downloadTicket(data.redemption_id, data.download_token)
    if (single)
      downloads.push(single)
  }
  if (downloads.length === 0)
    throw new GuanlanCdkError('观澜兑换结果不完整，请稍后重试')

  return { idempotencyKey, downloads }
}

function downloadTicket(redemptionId?: string, downloadToken?: string): DownloadTicket | null {
  const id = redemptionId?.trim() ?? ''
  const token = downloadToken?.trim() ?? ''
  if (!REDEMPTION_ID_PATTERN.test(id) || !token)
    return null
  return { redemptionId: id, downloadToken: token }
}

async function downloadExport(ticket: DownloadTicket, client: string) {
  const data = await guanlanRequest<Record<string, unknown>>(
    `/api/cdk/redemptions/${encodeURIComponent(ticket.redemptionId)}/download?format=${DOWNLOAD_FORMAT}`,
    {
      method: 'GET',
      headers: {
        'x-cdk-client': client,
        'x-cdk-client-version': CLIENT_VERSION,
        'x-cdk-download-token': ticket.downloadToken,
      },
    },
  )
  if (!Array.isArray(data.accounts) || data.accounts.length === 0 || data.ok === false)
    throw new GuanlanCdkError('观澜未返回可导入的账号文件')
  return data
}

/** 接收回执（观澜 `durable_delivery_receipts`）：与兑换请求同一 Idempotency-Key。 */
async function acknowledgeReceipt(ticket: DownloadTicket, client: string, idempotencyKey: string) {
  try {
    await guanlanRequest<Record<string, unknown>>(
      `/api/cdk/redemptions/${encodeURIComponent(ticket.redemptionId)}/received`,
      {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'idempotency-key': idempotencyKey,
          'x-cdk-client': client,
          'x-cdk-client-version': CLIENT_VERSION,
        },
        body: '{}',
      },
    )
  }
  catch {
    // 回执失败不阻断导入。
  }
}

async function guanlanRequest<T>(path: string, init: RequestInit): Promise<T> {
  let response: Response
  try {
    response = await fetch(`${GUANLAN_BASE}${path}`, init)
  }
  catch {
    throw new GuanlanCdkError('无法连接观澜兑换服务', true)
  }

  let data: unknown = null
  try {
    data = await response.json()
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
        cdk_client_version: typeof payload.cdk_client_version === 'string' ? payload.cdk_client_version : undefined,
      }),
      response.status >= 500,
    )
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

function publicRedeemError(payload: { error?: string, error_code?: string, cdk_client_version?: string }) {
  const code = typeof payload.error_code === 'string' ? payload.error_code : ''
  // 观澜兑换协议升级后旧版本会被 409 拒绝，且「本次未执行兑换」，需要升级 CPR。
  if (code === 'client_update_required') {
    const required = CLIENT_VERSION_PATTERN.test(payload.cdk_client_version ?? '')
      ? payload.cdk_client_version
      : '未知'
    return `观澜 CDK 兑换接口已升级（要求版本 ${required}，当前 ${CLIENT_VERSION}），本次未执行兑换，请联系管理员升级 CPR`
  }
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
  cdk_client_version?: string
  downloads?: Array<{
    redemption_id?: string
    download_token?: string
  }>
}
