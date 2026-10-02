import type { Ref } from 'vue'
import type { Account, OAuthStateConfiguration } from '@/api/modules/accounts'
import { useIntervalFn } from '@vueuse/core'
import { shallowRef, watch } from 'vue'
import { getAccountDetail } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { errorMessage } from '@/utils/async'

interface ConfigurationEntry {
  value?: OAuthStateConfiguration
  loading: boolean
  error?: string
}

export function useAccountConfigurations(accounts: Ref<Account[]>) {
  const entries = shallowRef<Record<string, ConfigurationEntry>>({})
  const request = useRequestState()

  async function reload() {
    const requestId = request.start()
    const signal = request.signal
    const rows = accounts.value.filter(account => account.provider === 'openai' && account.authenticationKind === 'oauth')
    entries.value = Object.fromEntries(rows.map(account => [account.id, {
      value: entries.value[account.id]?.value,
      loading: true,
    }]))
    let next = 0
    // 复用详情的脱敏配置合同，并限制并发，避免大页账号同时请求。
    await Promise.all(Array.from({ length: Math.min(4, rows.length) }, async () => {
      while (request.isCurrent(requestId)) {
        const account = rows[next++]
        if (!account)
          break
        try {
          const detail = await getAccountDetail({ accountId: account.id }, { signal, silent: true })
          if (!request.isCurrent(requestId))
            return
          const value = detail.credentialConfiguration
          if (!value || !('pinTurnState' in value))
            throw new Error('该账号暂不支持 state 配置')
          entries.value = { ...entries.value, [account.id]: { value, loading: false } }
        }
        catch (error) {
          if (!request.isCurrent(requestId))
            return
          entries.value = { ...entries.value, [account.id]: { loading: false, error: errorMessage(error, '读取失败') } }
        }
      }
    }))
    request.finish(requestId)
  }

  // 列表会定时静默刷新；只有账号集合变化（翻页、筛选、增删）才需要重读配置，
  // 其余由下面的定时器负责，避免每次列表刷新都对每个账号发一轮请求。
  watch(() => accounts.value.map(account => account.id).join(','), () => {
    void reload()
  }, { immediate: true })
  useIntervalFn(() => {
    if (!document.hidden && !request.loading.value)
      void reload()
  }, 30_000)

  return { entries, reload }
}
