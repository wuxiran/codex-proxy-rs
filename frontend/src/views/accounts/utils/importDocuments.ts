// 账号导入 JSON 的解析：把一份文本解析成一个或多个「Provider 文档」。
// 从 useAccountOnboarding 抽出，供提交流程与「账号文件」多文件合并共用。

import { isRecord } from '@/utils/object'
import { formatProviderLabel, isSupportedProvider } from '@/utils/providers'

export type ImportProvider = 'openai' | 'xai'

export interface MixedImportDocument {
  provider: ImportProvider
  document: Record<string, unknown>
}

export function isSub2apiAccountExport(value: unknown): value is Record<string, unknown> {
  if (!isRecord(value))
    return false
  const nested = isRecord(value.data) ? value.data : null
  const accounts = Array.isArray(value.accounts)
    ? value.accounts
    : nested && Array.isArray(nested.accounts)
      ? nested.accounts
      : null
  if (!Array.isArray(accounts) || accounts.length === 0)
    return false
  return accounts.some((account) => {
    if (!isRecord(account))
      return false
    const platform = typeof account.platform === 'string' ? account.platform : typeof account.provider === 'string' ? account.provider : ''
    const kind = typeof account.type === 'string' ? account.type : ''
    return platform.toLowerCase() === 'openai'
      || platform.toLowerCase() === 'codex'
      || kind.toLowerCase() === 'oauth'
      || kind.toLowerCase() === 'openai'
      || kind.toLowerCase() === 'codex'
  })
}

export function parseImportJson(value: string) {
  try {
    return JSON.parse(value)
  }
  catch {
    throw new Error('JSON 格式不正确')
  }
}

export function providerImportDocuments(value: unknown, provider: ImportProvider): MixedImportDocument[] {
  if (isSub2apiAccountExport(value)) {
    if (provider !== 'openai')
      throw new Error('Sub2API 导出只包含 OpenAI / Codex 账号，请选择 OpenAI')
    return [{ provider: 'openai', document: value }]
  }
  if (isRecord(value) && Array.isArray(value.documents)) {
    const documents = parseMixedImportDocuments(value)
      .filter(entry => entry.provider === provider)
    if (documents.length === 0) {
      const label = formatProviderLabel(provider)
      throw new Error(`批量导入文件不包含 ${label} 账号文档`)
    }
    return documents
  }
  if (!isRecord(value))
    throw new Error('导入文件必须是 JSON object')
  return [{ provider, document: value }]
}

export function parseMixedImportDocuments(value: unknown): MixedImportDocument[] {
  if (isSub2apiAccountExport(value))
    return [{ provider: 'openai', document: value }]
  if (!isRecord(value) || !Array.isArray(value.documents))
    throw new Error('批量导入文件必须是 CPR 多平台导出或 Sub2API 账号导出')

  const documents: MixedImportDocument[] = []
  for (const entry of value.documents) {
    if (!isRecord(entry))
      throw new Error('批量导入文件包含无效的 Provider 文档')
    const provider = entry.provider
    if (!isSupportedProvider(provider))
      throw new Error('批量导入文件包含无效的 Provider 文档')
    if (!isRecord(entry.document))
      throw new Error('批量导入文件包含无效的 Provider 文档')
    documents.push({ provider, document: entry.document })
  }

  if (documents.length === 0)
    throw new Error('批量文件没有可导入的账号文档')
  return documents
}

/**
 * 把多份「账号文件」合并成一个 CPR 多平台导出信封 `{ documents: [...] }`。
 * 每份文件按所选 provider 走 {@link providerImportDocuments} 分类（单账号 / Sub2API 导出 /
 * CPR 多平台导出都支持），再把结果拼在一起——提交流程解析这个信封即得到全部账号文档。
 * 任一文件解析失败会抛出带该文件名的错误。
 */
export function combineAccountFilesToEnvelope(
  provider: ImportProvider,
  files: { name: string, text: string }[],
): string {
  const documents: MixedImportDocument[] = []
  for (const file of files) {
    try {
      documents.push(...providerImportDocuments(parseImportJson(file.text), provider))
    }
    catch (error) {
      const reason = error instanceof Error ? error.message : '无法解析'
      throw new Error(`文件「${file.name}」：${reason}`)
    }
  }
  return JSON.stringify({ documents }, null, 2)
}
