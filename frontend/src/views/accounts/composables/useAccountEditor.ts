import type { Ref } from 'vue'
import type { AccountModelAccess, ApiKeyConfiguration, getAccounts, OAuthStateConfiguration, TurnStateAutoHunt, TurnStateCaptureRule, TurnStatePinStatus } from '@/api'

import { toast } from '@codex-proxy/ui'
import { computed, ref, shallowRef, watch } from 'vue'
import { getAccountDetail, updateAccount, updateAccountTurnState } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useRequestState } from '@/composables/useRequestState'
import { accountModelAccessError } from '../utils/modelAccess'
import { concurrencyLimitInput, parseAccountSchedulingForm } from '../utils/schedulingForm'
import { apiKeyAccountError, emptyApiKeyAccountForm, isOpenAiApiKeyAccount, isOpenAiOAuthAccount, parseApiKeyConfiguration } from '../utils/upstreamApiKey'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]

export function useAccountEditor(options: {
  accounts: Ref<AccountRow[]>
  reloadAccounts: () => Promise<unknown>
  reloadGroups: () => Promise<unknown>
}) {
  const showEditModal = shallowRef(false)
  const editingAccountId = shallowRef<string | null>(null)
  const pinTurnState = shallowRef(false)
  const savedPinTurnState = shallowRef(false)
  const recaptureTurnState = shallowRef(false)
  const turnStatePins = ref<TurnStatePinStatus[]>([])
  const turnStateCaptureRule = ref<TurnStateCaptureRule | null>(null)
  const turnStateAutoHunt = ref<TurnStateAutoHunt | null>(null)
  const notes = shallowRef('')
  const schedulingEnabled = shallowRef(true)
  const concurrencyLimit = shallowRef('')
  const weight = shallowRef('1')
  const modelAccess = ref<AccountModelAccess | undefined>()
  const proxyMode = shallowRef('preserve')
  const proxyId = shallowRef('')
  const selectedGroupIds = ref<string[]>([])
  const saveAction = useAsyncAction()
  const saving = saveAction.loading
  const apiKey = ref(emptyApiKeyAccountForm())
  const configurationRequest = useRequestState()
  const configurationLoading = configurationRequest.loading
  const configurationReady = shallowRef(false)
  const savedConfiguration = shallowRef<ApiKeyConfiguration>()
  const oauthTransport = shallowRef<ApiKeyConfiguration['transport']>('prefer_websocket')
  const savedOAuthTransport = shallowRef<ApiKeyConfiguration['transport']>('prefer_websocket')

  async function loadConfiguration(accountId: string) {
    const requestId = configurationRequest.start()
    try {
      const detail = await getAccountDetail({ accountId }, { signal: configurationRequest.signal })
      if (!configurationRequest.isCurrent(requestId))
        return
      if (isOpenAiOAuthAccount(detail.account)) {
        const configuration = detail.credentialConfiguration
        const transport = configuration?.transport
        // fork：OAuth 账号的上游设置同时携带「固定自身 state」状态。
        const state = oauthStateConfiguration(configuration)
        if (transport !== 'http' && transport !== 'prefer_websocket' && !state)
          throw new Error('该账号没有 OAuth 上游设置')
        if (transport === 'http' || transport === 'prefer_websocket') {
          oauthTransport.value = transport
          savedOAuthTransport.value = transport
        }
        if (state) {
          pinTurnState.value = state.pinTurnState
          savedPinTurnState.value = state.pinTurnState
          turnStatePins.value = state.turnStatePins
          turnStateCaptureRule.value = state.turnStateCaptureRule ?? null
          turnStateAutoHunt.value = state.turnStateAutoHunt ?? null
        }
        configurationReady.value = true
        return
      }
      const configuration = parseApiKeyConfiguration(detail.credentialConfiguration)
      if (!configuration)
        throw new Error('该账号没有 API Key 上游设置')
      apiKey.value = { ...emptyApiKeyAccountForm(), ...configuration }
      savedConfiguration.value = configuration
      configurationReady.value = true
    }
    catch (error) {
      configurationRequest.fail(requestId, error)
    }
    finally {
      configurationRequest.finish(requestId)
    }
  }

  /** 遍历命中后服务端已改了绑定并钉住 state：刷新展示，并防止随后的「保存」把它们冲掉。 */
  async function setTurnStateAutoHunt(autoRenew: TurnStateAutoHunt | null) {
    const accountId = editingAccountId.value
    if (!accountId)
      return
    // 只改续期参数，不带 pinTurnState：重新提交开关会更换世代、作废刚钉住的 state。
    await updateAccountTurnState({
      accountId,
      turnStateAutoHunt: autoRenew ? { enabled: true, ...autoRenew } : { enabled: false },
    })
    await loadConfiguration(accountId)
  }

  /** 取消时服务端可能已经提交：稍等它收尾，再按实际状态刷新绑定与固定列表。 */
  async function afterTurnStateHuntCancelled() {
    const accountId = editingAccountId.value
    if (!accountId)
      return
    await new Promise(resolve => setTimeout(resolve, 1500))
    if (editingAccountId.value !== accountId)
      return
    proxyMode.value = 'preserve'
    proxyId.value = ''
    void options.reloadAccounts()
    await loadConfiguration(accountId)
  }

  async function afterTurnStateHunt(boundChanged: boolean, autoRenew: TurnStateAutoHunt | null) {
    const accountId = editingAccountId.value
    if (!accountId)
      return
    if (autoRenew || turnStateAutoHunt.value)
      await setTurnStateAutoHunt(autoRenew).catch(() => {})
    proxyMode.value = 'preserve'
    proxyId.value = ''
    recaptureTurnState.value = false
    if (boundChanged)
      void options.reloadAccounts()
    await loadConfiguration(accountId)
  }

  const editingAccount = computed(() => {
    const accountId = editingAccountId.value
    return accountId
      ? options.accounts.value.find(account => account.id === accountId) ?? null
      : null
  })

  function open(account: AccountRow) {
    configurationRequest.invalidate()
    editingAccountId.value = account.id
    notes.value = account.notes ?? ''
    pinTurnState.value = false
    savedPinTurnState.value = false
    recaptureTurnState.value = false
    turnStatePins.value = []
    turnStateCaptureRule.value = null
    turnStateAutoHunt.value = null
    proxyMode.value = 'preserve'
    proxyId.value = ''
    schedulingEnabled.value = account.enabled
    concurrencyLimit.value = concurrencyLimitInput(account.concurrencyLimit)
    weight.value = String(account.weight)
    modelAccess.value = { ...account.modelAccess, models: [...account.modelAccess.models] }
    selectedGroupIds.value = account.groups.map(group => group.id)
    apiKey.value = emptyApiKeyAccountForm()
    oauthTransport.value = 'prefer_websocket'
    savedOAuthTransport.value = 'prefer_websocket'
    savedConfiguration.value = undefined
    configurationReady.value = false
    showEditModal.value = true
    if (isOpenAiApiKeyAccount(account) || isOpenAiOAuthAccount(account))
      void loadConfiguration(account.id)
  }

  async function save() {
    const accountId = editingAccountId.value
    if (!accountId || saving.value)
      return
    const isApiKey = isOpenAiApiKeyAccount(editingAccount.value)
    const isOAuth = isOpenAiOAuthAccount(editingAccount.value)
    if (isApiKey && !configurationReady.value)
      return
    if (isApiKey) {
      const error = apiKeyAccountError(apiKey.value, true)
      if (error) {
        toast.warning(error)
        return
      }
    }
    const modelError = accountModelAccessError(modelAccess.value)
    if (modelError) {
      toast.warning(modelError)
      return
    }
    const scheduling = parseAccountSchedulingForm(concurrencyLimit.value, weight.value)
    if (proxyMode.value === 'proxy' && !proxyId.value.trim()) {
      toast.warning('请选择已通过测试的代理')
      return
    }
    if (!scheduling.valid) {
      toast.warning(scheduling.message)
      return
    }

    await saveAction.run(async () => {
      const settings = {
        accountId,
        notes: notes.value,
        outboundProxyId: proxyMode.value === 'preserve' ? undefined : proxyMode.value === 'direct' ? '' : proxyId.value.trim(),
        enabled: schedulingEnabled.value,
        concurrencyLimit: scheduling.values.concurrencyLimit,
        weight: scheduling.values.weight,
        modelAccess: modelAccess.value,
        groupIds: [...new Set(selectedGroupIds.value)],
      }
      const connectionChanged = isApiKey && (
        apiKey.value.apiKey !== ''
        || apiKey.value.base_url.trim() !== savedConfiguration.value?.base_url
        || apiKey.value.transport !== savedConfiguration.value?.transport
      )
      const connection = connectionChanged
        ? { baseUrl: apiKey.value.base_url.trim(), transport: apiKey.value.transport, apiKey: apiKey.value.apiKey || undefined }
        : isOAuth && configurationReady.value && oauthTransport.value !== savedOAuthTransport.value
          ? { transport: oauthTransport.value }
          : undefined
      // fork：「固定自身 state」开关走独立接口；没有连接变更时它顺带写入本次账号设置。
      const turnStateChanged = isOAuth && configurationReady.value && (pinTurnState.value !== savedPinTurnState.value || recaptureTurnState.value)
      if (turnStateChanged && !connection) {
        await updateAccountTurnState({ accountId, pinTurnState: pinTurnState.value, settings })
      }
      else {
        await updateAccount({ ...settings, connection })
        if (turnStateChanged)
          await updateAccountTurnState({ accountId, pinTurnState: pinTurnState.value })
      }
      showEditModal.value = false
      toast.success('账号已更新')
      void Promise.allSettled([options.reloadAccounts(), options.reloadGroups()])
    })
  }

  watch([showEditModal, saving], ([open, isSaving]) => {
    if (open || isSaving)
      return
    configurationRequest.invalidate()
    apiKey.value = emptyApiKeyAccountForm()
    oauthTransport.value = 'prefer_websocket'
    savedOAuthTransport.value = 'prefer_websocket'
    savedConfiguration.value = undefined
    configurationReady.value = false
    editingAccountId.value = null
    notes.value = ''
    proxyMode.value = 'preserve'
    proxyId.value = ''
    schedulingEnabled.value = true
    concurrencyLimit.value = ''
    weight.value = '1'
    modelAccess.value = undefined
    selectedGroupIds.value = []
  })

  return {
    apiKey,
    pinTurnState,
    savedPinTurnState,
    afterTurnStateHunt,
    afterTurnStateHuntCancelled,
    turnStateAutoHunt,
    stopTurnStateAutoHunt: () => setTurnStateAutoHunt(null),
    recaptureTurnState,
    turnStatePins,
    turnStateCaptureRule,
    oauthTransport,
    configurationLoading,
    configurationReady,
    showEditModal,
    editingAccount,
    notes,
    schedulingEnabled,
    concurrencyLimit,
    weight,
    modelAccess,
    proxyMode,
    proxyId,
    selectedGroupIds,
    saving,
    open,
    save,
  }
}

/** fork：OAuth 账号详情里的「固定自身 state」状态；旧后端或非 OpenAI OAuth 账号没有这些字段。 */
function oauthStateConfiguration(value: Record<string, unknown> | undefined): OAuthStateConfiguration | null {
  return value && typeof value.pinTurnState === 'boolean' && Array.isArray(value.turnStatePins)
    ? value as unknown as OAuthStateConfiguration
    : null
}
