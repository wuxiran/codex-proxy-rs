import type { AccountImportTask, getAccounts } from '@/api'

import { computed, ref, shallowRef, watch } from 'vue'
import {
  completeAccountOAuth,
  createAccountImportTask,
  GuanlanCdkError,
  importAccounts,
  parseGuanlanCdkCodes,
  redeemGuanlanCdks,
  startAccountOAuth,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { isRecord } from '@/utils/object'
import { formatProviderLabel, isSupportedProvider } from '@/utils/providers'
import { generateRequestId } from '@/utils/uuid'
import { accountImportSettings, accountProxyError, emptyAccountCreateForm } from '../components/AccountCreateModal/model'
import { apiKeyAccountError, emptyApiKeyAccountForm } from '../utils/upstreamApiKey'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
type ImportProvider = 'openai' | 'xai'

interface MixedImportDocument {
  provider: ImportProvider
  document: Record<string, unknown>
}

type OpenAiTokenImportMode = 'access_token' | 'refresh_token'

const MAX_TOKEN_IMPORT_COUNT = 200

export function useAccountOnboarding(options: {
  reload: () => Promise<unknown>
  onImportTaskCreated: (task: AccountImportTask) => void
}) {
  const createModalOpen = shallowRef(false)
  const reauthorizingAccount = shallowRef<AccountRow | null>(null)
  const creatingAccountAction = useAsyncAction()
  const authorizingOAuthAction = useAsyncAction()
  const creatingAccount = creatingAccountAction.loading
  const authorizingOAuth = authorizingOAuthAction.loading
  const createForm = ref(emptyAccountCreateForm())
  let submissionId: string | undefined
  watch(createForm, () => {
    submissionId = undefined
  }, { deep: true, flush: 'sync' })

  const showCreateModal = computed({
    get: () => createModalOpen.value,
    set: (value: boolean) => {
      createModalOpen.value = value
      if (!value) {
        reauthorizingAccount.value = null
        createForm.value = emptyAccountCreateForm()
      }
    },
  })

  async function handleCreate() {
    if (createForm.value.mode === 'oauth') {
      await completeOAuth()
      return
    }
    if (creatingAccount.value)
      return

    await creatingAccountAction.run(
      async () => {
        const proxyError = accountProxyError(createForm.value)
        if (proxyError)
          throw new Error(proxyError)
        const mode = createForm.value.mode
        if (mode === 'oauth')
          throw new Error('请选择凭据导入方式')
        if (mode === 'api_key') {
          const provider = requireImportProvider(createForm.value.provider)
          if (provider !== 'openai')
            throw new Error('当前平台不支持 API Key 账号')
          const form = createForm.value.apiKey
          const error = apiKeyAccountError(form)
          if (error)
            throw new Error(error)
          await importAccounts({
            provider,
            settings: accountImportSettings(createForm.value),
            outboundProxyId: createForm.value.proxyMode === 'proxy' ? createForm.value.proxyId.trim() : undefined,
            data: { provider, authentication_kind: 'api_key', name: form.name.trim(), base_url: form.base_url.trim(), api_key: form.apiKey, transport: form.transport },
          })
          await finishCreate('API Key 账号已添加')
          return
        }
        const documents = createForm.value.provider === 'batch'
          ? parseMixedImportDocuments(parseImportJson(createForm.value.importTexts.json))
          : await accountImportDocuments(
              requireImportProvider(createForm.value.provider),
              mode,
              createForm.value.importTexts[mode],
            )
        if (documents.length > MAX_TOKEN_IMPORT_COUNT)
          throw new Error(`单次最多导入 ${MAX_TOKEN_IMPORT_COUNT} 个条目`)
        submissionId ??= generateRequestId()
        const task = await createAccountImportTask({
          submissionId,
          items: documents.map(entry => ({
            provider: entry.provider,
            data: entry.document,
            settings: accountImportSettings(createForm.value),
            outboundProxyId: createForm.value.proxyMode === 'proxy' ? createForm.value.proxyId.trim() : undefined,
          })),
        })
        showCreateModal.value = false
        options.onImportTaskCreated(task)
        toast.success('导入任务已创建')
      },
    )
  }

  async function handleAuthorizeOAuth() {
    if (authorizingOAuth.value)
      return

    await authorizingOAuthAction.run(
      async () => {
        const input = newAccountInput()
        const account = reauthorizingAccount.value
        const proxyError = accountProxyError(createForm.value)
        if (!account && proxyError)
          throw new Error(proxyError)
        const result = await startAccountOAuth({
          ...input,
          outboundProxyId: !account && createForm.value.proxyMode === 'proxy' ? createForm.value.proxyId.trim() : undefined,
          ...(account
            ? {
                accountId: account.id,
              }
            : {}),
        })

        createForm.value = {
          ...createForm.value,
          oauthFlowId: result.flowId,
          oauthAuthUrl: result.authorizationUrl,
          oauthCallback: '',
        }
        toast.success('授权链接已生成')
      },
    )
  }

  async function completeOAuth() {
    if (creatingAccount.value)
      return

    await creatingAccountAction.run(
      async () => {
        if (!createForm.value.oauthFlowId)
          throw new Error('请先生成授权链接')

        const callbackUrl = createForm.value.oauthCallback.trim()
        if (!callbackUrl) {
          throw new Error(createForm.value.provider === 'xai'
            ? '请粘贴 OAuth 回调地址、含 code 和 state 的查询字符串或授权码'
            : '请粘贴 OAuth 回调地址')
        }
        await completeAccountOAuth({
          provider: createForm.value.provider,
          flowId: createForm.value.oauthFlowId,
          callbackUrl,
          settings: reauthorizingAccount.value ? undefined : accountImportSettings(createForm.value),
        })
        await finishCreate(
          reauthorizingAccount.value
            ? '账号重新授权成功'
            : createForm.value.provider === 'xai'
              ? 'xAI OAuth 账号已添加'
              : 'OpenAI OAuth 账号已添加',
        )
      },
    )
  }

  function openCreateAccount() {
    reauthorizingAccount.value = null
    createForm.value = emptyAccountCreateForm()
    showCreateModal.value = true
  }

  function openReauthorizeAccount(account: AccountRow) {
    if (account.authenticationKind !== 'oauth' || (account.provider !== 'openai' && account.provider !== 'xai'))
      return
    reauthorizingAccount.value = account
    createForm.value = {
      ...emptyAccountCreateForm(),
      provider: account.provider,
      step: 'import',
      mode: 'oauth',
    }
    showCreateModal.value = true
    void handleAuthorizeOAuth()
  }

  function newAccountInput() {
    const account = reauthorizingAccount.value
    return {
      provider: createForm.value.provider,
      name: account?.name || account?.email || `${createForm.value.provider} OAuth`,
    }
  }

  async function finishCreate(message: string) {
    showCreateModal.value = false
    await options.reload()
    toast.success(message)
  }

  watch(
    () => createForm.value.provider,
    () => {
      createForm.value = {
        ...createForm.value,
        mode: createForm.value.provider === 'batch' ? 'json' : 'oauth',
        apiKey: emptyApiKeyAccountForm(),
        importTexts: { access_token: '', refresh_token: '', json: '', cdk: '' },
        oauthFlowId: '',
        oauthAuthUrl: '',
        oauthCallback: '',
      }
    },
    { flush: 'sync' },
  )

  watch(
    [
      () => createForm.value.proxyMode,
      () => createForm.value.proxyMode === 'proxy' ? createForm.value.proxyId.trim() : '',
    ],
    () => {
      createForm.value.oauthFlowId = ''
      createForm.value.oauthAuthUrl = ''
      createForm.value.oauthCallback = ''
    },
    { flush: 'sync' },
  )

  return {
    showCreateModal,
    reauthorizingAccount,
    creatingAccount,
    authorizingOAuth,
    createForm,
    handleCreate,
    handleAuthorizeOAuth,
    openCreateAccount,
    openReauthorizeAccount,
  }
}

function isSub2apiAccountExport(value: unknown): value is Record<string, unknown> {
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

function parseImportJson(value: string) {
  try {
    return JSON.parse(value)
  }
  catch {
    throw new Error('JSON 格式不正确')
  }
}

function requireImportProvider(value: string): ImportProvider {
  if (isSupportedProvider(value))
    return value
  throw new Error('请选择要导入的账号平台')
}

async function accountImportDocuments(
  provider: ImportProvider,
  mode: string,
  value: string,
): Promise<MixedImportDocument[]> {
  if (provider === 'openai' && mode === 'cdk')
    return [{ provider, document: await redeemGuanlanImport(value) }]
  if (provider === 'openai' && isOpenAiTokenImportMode(mode)) {
    return parseOpenAiTokenImport(value, mode).map(document => ({ provider, document }))
  }
  return providerImportDocuments(parseImportJson(value), provider)
}

async function redeemGuanlanImport(value: string) {
  const cdks = parseGuanlanCdkCodes(value)
  try {
    return await redeemGuanlanCdks(cdks)
  }
  catch (error) {
    if (error instanceof GuanlanCdkError && error.network)
      return { cdks }
    throw error
  }
}

function parseOpenAiTokenImport(value: string, mode: OpenAiTokenImportMode) {
  const tokens = value
    .split(/\r?\n/)
    .map(token => token.trim())
    .filter(Boolean)
  const label = mode === 'access_token' ? 'Access Token' : 'Refresh Token'

  if (tokens.length === 0)
    throw new Error(`请至少粘贴一个 ${label}`)
  if (tokens.length > MAX_TOKEN_IMPORT_COUNT)
    throw new Error(`单次最多导入 ${MAX_TOKEN_IMPORT_COUNT} 个 ${label}`)

  const credentialKey = mode === 'access_token' ? 'accessToken' : 'refreshToken'
  return tokens.map(token => ({ accounts: [{ [credentialKey]: token }] }))
}

function isOpenAiTokenImportMode(value: string): value is OpenAiTokenImportMode {
  return value === 'access_token' || value === 'refresh_token'
}

function providerImportDocuments(value: unknown, provider: ImportProvider): MixedImportDocument[] {
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

function parseMixedImportDocuments(value: unknown): MixedImportDocument[] {
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
