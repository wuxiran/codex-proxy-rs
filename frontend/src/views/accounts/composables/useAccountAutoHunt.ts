import type { TurnStateAutoHuntParam, TurnStateHuntEvent } from '@/api'
import { computed, onScopeDispose, ref } from 'vue'
import { turnStateAutoHuntStreamUrl } from '@/api'

export type AutoHuntStatus = 'idle' | 'running' | 'finalizing' | 'success' | 'miss' | 'error' | 'cancelled'

/**
 * 自动撞 state 的 SSE 会话。事件形状与遍历完全一致，但这里没有固定代理列表可做行——
 * 每次尝试都是一个新的临时 IP，所以把事件聚合成滚动计数（已试 IP、已发请求、按国家分布）。
 */
export function useAccountAutoHunt() {
  const status = ref<AutoHuntStatus>('idle')
  const expectedLength = ref<number | null>(null)
  const totalPlanned = ref(0)
  const ipsTried = ref(0)
  const requests = ref(0)
  const hits = ref(0)
  const currentCountry = ref('')
  const lastLength = ref<number | null>(null)
  const countryTally = ref<Record<string, number>>({})
  const message = ref('')
  const boundChanged = ref(false)
  let source: EventSource | null = null

  const countryRows = computed(() =>
    Object.entries(countryTally.value).sort(([a], [b]) => a.localeCompare(b)))

  function close() {
    source?.close()
    source = null
  }

  /** "US · 动态" → "US"；拿国家做分布统计。 */
  function countryOf(name: string) {
    return name.split(' ')[0] ?? name
  }

  function handle(event: TurnStateHuntEvent) {
    switch (event.type) {
      case 'hunt_start':
        expectedLength.value = event.expectedLength
        totalPlanned.value = event.proxies.length
        break
      case 'proxy_start': {
        const country = countryOf(event.name)
        currentCountry.value = country
        countryTally.value[country] = (countryTally.value[country] ?? 0) + 1
        break
      }
      case 'attempt':
        requests.value += 1
        lastLength.value = event.length
        break
      case 'proxy_done':
        ipsTried.value += 1
        break
      case 'hit':
        hits.value += 1
        status.value = 'finalizing'
        break
      case 'bound':
        boundChanged.value = event.changed
        break
      case 'pinned':
        message.value = `已钉住 ${event.model} 的 ${event.length} 字节 state，${new Date(event.expiresAt).toLocaleString()} 到期；账号已切到静态出口`
        break
      case 'hunt_complete':
        requests.value = event.requests
        status.value = event.success ? 'success' : 'miss'
        if (!event.success)
          message.value = `试了 ${ipsTried.value} 个 IP 都没撞到符合规则的 state，账号设置未改动`
        close()
        break
      case 'error':
        status.value = 'error'
        message.value = event.message
        close()
        break
    }
  }

  function start(params: TurnStateAutoHuntParam) {
    close()
    status.value = 'running'
    expectedLength.value = null
    totalPlanned.value = 0
    ipsTried.value = 0
    requests.value = 0
    hits.value = 0
    currentCountry.value = ''
    lastLength.value = null
    countryTally.value = {}
    message.value = ''
    boundChanged.value = false
    const stream = new EventSource(turnStateAutoHuntStreamUrl(params), { withCredentials: true })
    source = stream
    stream.onmessage = (raw) => {
      try {
        handle(JSON.parse(raw.data) as TurnStateHuntEvent)
      }
      catch {
        // 保活帧或无法解析的帧不影响。
      }
    }
    stream.onerror = () => {
      if (source !== stream)
        return
      close()
      if (status.value === 'running' || status.value === 'finalizing') {
        status.value = 'error'
        message.value = ipsTried.value > 0
          ? '连接中断；若已命中，绑定与钉住仍会在服务端完成，请重新打开账号查看'
          : '无法开始：请确认已开启并保存「固定自身 state」、选了轮换代理模板与至少一个测试通过的静态出口，且该账号没有其它撞/遍历在运行'
      }
    }
  }

  function cancel() {
    if (status.value !== 'running')
      return
    close()
    status.value = 'cancelled'
    message.value = '已停止。若停止的瞬间恰好命中，服务端可能仍在完成绑定与钉住；请稍后重新打开账号确认'
  }

  onScopeDispose(close)

  return {
    status,
    expectedLength,
    totalPlanned,
    ipsTried,
    requests,
    hits,
    currentCountry,
    lastLength,
    countryRows,
    message,
    boundChanged,
    start,
    cancel,
  }
}
