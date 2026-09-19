export type ImportAccount = Record<string, unknown>

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * 把粘贴或上传的 JSON 拆成单账号条目，页面逐个提交以展示进度。
 * 服务端会重新拆分并剥离代理字段，这里只负责展示用的切分。
 */
export function splitImportDocument(text: string, maxAccounts: number): ImportAccount[] {
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  }
  catch {
    throw new Error('不是合法的 JSON，请检查文件内容')
  }
  if (!isObject(parsed))
    throw new Error('JSON 顶层必须是对象')

  // sub2api 导出接口的响应信封：{ code, message, data: { accounts, proxies } }
  const payload = isObject(parsed.data) && 'accounts' in parsed.data ? parsed.data : parsed
  const accounts = 'accounts' in payload ? payload.accounts : [payload]
  if (!Array.isArray(accounts))
    throw new Error('accounts 必须是数组')
  if (accounts.length === 0)
    throw new Error('文件中没有账号')
  if (accounts.length > maxAccounts)
    throw new Error(`单次最多导入 ${maxAccounts} 个账号，当前 ${accounts.length} 个，请拆分后再试`)
  if (!accounts.every(isObject))
    throw new Error('账号条目必须是 JSON 对象')
  return accounts
}

export function importAccountLabel(account: ImportAccount, index: number): string {
  const credentials = isObject(account.credentials) ? account.credentials : {}
  const label = [account.name, account.label, account.email, credentials.email]
    .find((value): value is string => typeof value === 'string' && value.trim() !== '')
  return label?.trim() ?? `账号 ${index + 1}`
}
