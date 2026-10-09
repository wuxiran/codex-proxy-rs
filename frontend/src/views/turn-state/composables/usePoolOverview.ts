import type { PoolSnapshot } from '@/api'
import { computed, onBeforeUnmount, onMounted, ref, shallowRef } from 'vue'
import { getTurnStatePool } from '@/api'

export function usePoolOverview() {
  const snapshot = shallowRef<PoolSnapshot | null>(null)
  const loading = ref(false)
  const error = ref('')
  const paused = ref(false)
  const now = ref(Date.now())
  let observedTime = Date.now()
  let observedClock = performance.now()
  const currentTime = () => observedTime + performance.now() - observedClock
  let timer: ReturnType<typeof setTimeout> | undefined
  let ticker: ReturnType<typeof setInterval> | undefined
  let expiryTimer: ReturnType<typeof setTimeout> | undefined
  let request: AbortController | undefined
  let mounted = false
  let generation = 0
  let pending = false
  let focused = document.hasFocus()
  const active = () => mounted && !document.hidden && focused
  const stale = computed(() => !snapshot.value || now.value >= snapshot.value.validUntilMs)
  const accounts = computed(() => Object.fromEntries(snapshot.value?.accounts.map(account => [account.accountId, account]) ?? []))

  function watchExpiry() {
    clearTimeout(expiryTimer)
    if (!active() || paused.value || !snapshot.value)
      return
    const delay = snapshot.value.validUntilMs - currentTime()
    if (delay <= 0) {
      now.value = currentTime()
      return
    }
    expiryTimer = setTimeout(() => {
      if (active() && !paused.value) {
        now.value = currentTime()
        watchExpiry()
      }
    }, Math.ceil(delay))
  }

  async function refresh() {
    clearTimeout(timer)
    if (!active())
      return
    if (loading.value) {
      pending = true
      return
    }
    loading.value = true
    pending = false
    const current = generation
    const controller = new AbortController()
    const started = performance.now()
    request = controller
    try {
      const result = await getTurnStatePool({ signal: controller.signal, timeout: 6000, silent: true })
      if (!result || !Array.isArray(result.accounts) || !Array.isArray(result.gateways)
        || !Number.isFinite(result.observedAtMs) || !Number.isFinite(result.validUntilMs)
        || result.validUntilMs <= result.observedAtMs || result.scope !== 'current_process'
        || !result.totals || !['gateways', 'tickets', 'connections', 'availableConnections', 'verifiedConnections', 'preparingAccounts', 'coolingAccounts']
        .every(key => Number.isSafeInteger(result.totals[key as keyof PoolSnapshot['totals']]) && result.totals[key as keyof PoolSnapshot['totals']] >= 0)) {
        throw new Error('运行状态不完整')
      }
      if (current !== generation || !active())
        return
      snapshot.value = result
      observedClock = performance.now()
      observedTime = result.observedAtMs + observedClock - started
      now.value = currentTime()
      paused.value = false
      error.value = ''
      watchExpiry()
    }
    catch (cause) {
      if (current === generation && !controller.signal.aborted) {
        error.value = cause instanceof Error ? cause.message : '读取票池失败'
        paused.value = false
        now.value = currentTime()
        watchExpiry()
      }
    }
    finally {
      loading.value = false
      if (request === controller)
        request = undefined
      if (active())
        timer = setTimeout(() => void refresh(), pending ? 0 : 2000)
    }
  }

  function visibility() {
    generation++
    clearTimeout(timer)
    clearTimeout(expiryTimer)
    request?.abort()
    paused.value = true
    if (active()) {
      now.value = currentTime()
      void refresh()
    }
  }
  function onBlur() {
    focused = false
    visibility()
  }
  function onFocus() {
    focused = true
    visibility()
  }

  onMounted(() => {
    mounted = true
    document.addEventListener('visibilitychange', visibility)
    window.addEventListener('focus', onFocus)
    window.addEventListener('blur', onBlur)
    ticker = setInterval(() => {
      if (active() && !paused.value)
        now.value = currentTime()
    }, 1000)
    visibility()
  })
  onBeforeUnmount(() => {
    mounted = false
    generation++
    request?.abort()
    clearTimeout(timer)
    clearInterval(ticker)
    clearTimeout(expiryTimer)
    document.removeEventListener('visibilitychange', visibility)
    window.removeEventListener('focus', onFocus)
    window.removeEventListener('blur', onBlur)
  })
  return { snapshot, accounts, loading, error, paused, stale, now, refresh }
}
