import type { AccountImportInputMode } from '../utils/accountCreate'
import type { ImportProvider } from '../utils/importDocuments'
import type { Account, AccountImportTask } from '@/api'
import { ZNotification } from '@codex-proxy/ui'
import { computed, ref, shallowRef, watch } from 'vue'
import {
  createAccountImportTask,
  GuanlanCdkError,
  importAccounts,
  parseGuanlanCdkCodes,
  redeemGuanlanCdks,
} from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { errorMessage, generateRequestId } from '@/utils/operation'
import { formatProviderLabel, isSupportedProvider } from '@/utils/providers'
import { accountCreateProvider, accountCreateSourceKey, accountImportModes, accountImportSettings, accountProxyError, emptyAccountCreateForm } from '../utils/accountCreate'
import { accountImportBatches, accountImportDocuments, mixedImportDocuments } from '../utils/accountImport'
import { isSub2apiAccountExport, parseImportJson } from '../utils/importDocuments' // fork: sub2api-import
import { apiKeyAccountError, emptyApiKeyAccountForm } from '../utils/upstreamApiKey'
import { useAccountAuthorization } from './useAccountAuthorization'

export function useAccountOnboarding(options: {
  reload: () => Promise<unknown>
  onImportTaskCreated: (task: AccountImportTask, reveal?: boolean) => void
}) {
  const createModalOpen = shallowRef(false)
  const reauthorizingAccount = shallowRef<Account | null>(null)
  const creatingAccountAction = useAsyncAction()
  const createForm = ref(emptyAccountCreateForm())
  const authorization = useAccountAuthorization(() => finishCreate(reauthorizingAccount.value ? '账号重新授权成功' : '账号已添加'))
  let submissionIds: string[] = []
  watch(createForm, () => {
    submissionIds = []
  }, { deep: true, flush: 'sync' })

  const showCreateModal = computed({
    get: () => createModalOpen.value,
    set: (value: boolean) => {
      createModalOpen.value = value
      if (!value)
        authorization.cancel()
    },
  })

  function clearCreate() {
    authorization.reset()
    reauthorizingAccount.value = null
    createForm.value = emptyAccountCreateForm()
  }

  function requireImportProvider(provider: string | undefined) {
    if (!provider)
      throw new Error('请选择账号平台')
    if (!isSupportedProvider(provider))
      throw new Error('账号平台不受支持')
    return provider
  }

  async function handleCreate() {
    if (createForm.value.mode === 'oauth') {
      await authorization.complete()
      return
    }
    let submitted = 0
    await creatingAccountAction.run(async () => {
      const form = createForm.value
      const proxyError = accountProxyError(form)
      if (proxyError)
        throw new Error(proxyError)
      const settings = accountImportSettings(form)
      const outboundProxyId = form.proxyMode === 'proxy' ? form.proxyId.trim() : undefined
      const mode = form.mode
      if (mode === 'oauth')
        return
      const provider = accountCreateProvider(form)
      if (mode === 'api_key') {
        if (requireImportProvider(provider) !== 'openai')
          throw new Error('当前平台不支持 API Key 账号')
        const error = apiKeyAccountError(form.apiKey)
        if (error)
          throw new Error(error)
        await importAccounts({
          provider: 'openai',
          settings,
          outboundProxyId,
          data: { provider: 'openai', authentication_kind: 'api_key', name: form.apiKey.name.trim(), base_url: form.apiKey.base_url.trim(), api_key: form.apiKey.apiKey, transport: form.apiKey.transport },
        })
        await finishCreate('API Key 账号已添加')
        return
      }
      // fork：批量文件额外识别 Sub2API 账号导出，单平台支持观澜 CDK 兑换。
      const documents = form.source?.kind === 'bundle'
        ? bundleImportDocuments(form.importTexts.json)
        : await providerImportDocumentsFor(requireImportProvider(provider), mode, form.importTexts[mode])
      for (const document of documents)
        requireImportProvider(document.provider)
      const batches = accountImportBatches(documents.map(entry => ({ provider: entry.provider, data: entry.document, settings, outboundProxyId })))
      // 同一输入重试时复用每批标识，恢复丢失响应或部分提交，不重复执行 RT 交换
      const ids = submissionIds
      for (const [index, items] of batches.entries()) {
        const task = await createAccountImportTask({ submissionId: ids[index] ??= generateRequestId(), items })
        submitted += 1
        const finished = index === batches.length - 1
        if (finished)
          showCreateModal.value = false
        options.onImportTaskCreated(task, finished)
      }
      ZNotification.success({ message: submitted > 1 ? `已创建 ${submitted} 个导入任务` : '导入任务已创建' })
    }, {
      onError: () => {
        if (submitted)
          ZNotification.warning({ message: '部分导入任务已创建，保持输入不变重试可继续提交' })
      },
    })
  }

  async function handleAuthorizeOAuth() {
    if (authorization.busy.value)
      return
    try {
      const form = createForm.value
      const provider = accountCreateProvider(form)
      if (!isSupportedProvider(provider))
        throw new Error('账号平台不受支持')
      const account = reauthorizingAccount.value
      const proxyError = accountProxyError(form)
      if (proxyError)
        throw new Error(proxyError)
      await authorization.start({
        start: {
          provider,
          name: account?.name || account?.email || `${formatProviderLabel(provider)} 账号`,
          accountId: account?.id,
          outboundProxyId: account ? undefined : form.proxyMode === 'proxy' ? form.proxyId.trim() : undefined,
        },
        settings: account ? undefined : accountImportSettings(form),
      })
    }
    catch (cause) {
      ZNotification.error({ message: errorMessage(cause, '启动授权失败') })
    }
  }

  function openCreateAccount() {
    authorization.reset()
    reauthorizingAccount.value = null
    createForm.value = emptyAccountCreateForm()
    showCreateModal.value = true
  }

  function openReauthorizeAccount(account: Account) {
    if (isSupportedProvider(account.provider) && account.authenticationKind !== 'oauth')
      return
    authorization.reset()
    reauthorizingAccount.value = account
    createForm.value = { ...emptyAccountCreateForm(), source: { kind: 'provider', id: account.provider }, step: 'import' }
    showCreateModal.value = true
    void handleAuthorizeOAuth()
  }

  async function finishCreate(message: string) {
    showCreateModal.value = false
    ZNotification.success({ message })
    await options.reload()
  }

  watch(() => accountCreateSourceKey(createForm.value), () => {
    createForm.value = {
      ...createForm.value,
      mode: reauthorizingAccount.value ? 'oauth' : createForm.value.source?.kind === 'bundle' ? 'json' : accountImportModes(accountCreateProvider(createForm.value))[0]?.value ?? 'json',
      apiKey: emptyApiKeyAccountForm(),
      importTexts: { access_token: '', refresh_token: '', json: '', cdk: '' },
    }
  }, { flush: 'sync' })

  watch([
    () => accountCreateSourceKey(createForm.value),
    () => createForm.value.mode,
    () => createForm.value.step,
    () => createForm.value.proxyMode,
    () => createForm.value.proxyMode === 'proxy' ? createForm.value.proxyId.trim() : '',
  ], () => {
    authorization.reset()
  }, { flush: 'sync' })

  return {
    showCreateModal,
    reauthorizingAccount,
    creatingAccount: creatingAccountAction.loading,
    authorizingOAuth: authorization.busy,
    authorization: authorization.view,
    authorizationCallback: authorization.callback,
    createForm,
    handleCreate,
    handleAuthorizeOAuth,
    openCreateAccount,
    openReauthorizeAccount,
    clearCreate,
  }
}

type ImportDocument = ReturnType<typeof accountImportDocuments>[number]

/** fork：批量文件先识别 Sub2API 账号导出（整份交给 Provider）；其余交给上游解析，含逐账号拆分。 */
function bundleImportDocuments(text: string): ImportDocument[] {
  const value = parseImportJson(text)
  if (isSub2apiAccountExport(value))
    return [{ provider: 'openai', document: value }]
  return mixedImportDocuments(text)
}

/**
 * fork：单平台导入的文档解析。观澜 CDK 先兑换成已签名账号；「账号文件」识别 Sub2API 导出；
 * CPR 多平台导出与 AT/RT 等其余模式沿用上游解析。
 */
async function providerImportDocumentsFor(
  provider: ImportProvider,
  mode: AccountImportInputMode,
  value: string,
): Promise<ImportDocument[]> {
  if (mode === 'cdk') {
    if (provider !== 'openai')
      throw new Error('观澜 CDK 只能导入 OpenAI 账号')
    // 多空间兑换时每个空间一份已签名文件，逐份导入（签名按文件计算，不能合并）。
    const documents = await redeemGuanlanImport(value)
    return documents.map(document => ({ provider, document }))
  }
  if (mode === 'json' && isSub2apiAccountExport(parseImportJson(value))) {
    if (provider !== 'openai')
      throw new Error('Sub2API 导出只包含 OpenAI / Codex 账号，请选择 OpenAI')
    return [{ provider, document: parseImportJson(value) }]
  }
  return accountImportDocuments(provider, mode, value)
}

async function redeemGuanlanImport(value: string) {
  const cdks = parseGuanlanCdkCodes(value)
  try {
    return await redeemGuanlanCdks(cdks)
  }
  catch (error) {
    if (error instanceof GuanlanCdkError && error.network)
      return [{ cdks }]
    throw error
  }
}
