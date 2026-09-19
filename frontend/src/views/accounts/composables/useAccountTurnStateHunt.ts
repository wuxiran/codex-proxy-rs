import type { TurnStateHuntAttemptError, TurnStateHuntEvent, TurnStateHuntParam, TurnStateHuntProxy } from '@/api'
import { onScopeDispose, ref, shallowRef } from 'vue'
import { turnStateHuntStreamUrl } from '@/api'

export interface TurnStateHuntAttempt {
  index: number
  length: number | null
  matched: boolean
  error: TurnStateHuntAttemptError | null
}

export interface TurnStateHuntRow extends TurnStateHuntProxy {
  key: string
  state: 'pending' | 'running' | 'hit' | 'miss' | 'skipped'
  skipped: 'unavailable' | 'unreachable' | null
  attempts: TurnStateHuntAttempt[]
}

export type TurnStateHuntStatus = 'idle' | 'running' | 'finalizing' | 'success' | 'miss' | 'error' | 'cancelled'

const rowKey = (proxyId: string | null) => proxyId ?? 'direct'

/** 遍历代理找 state 的 SSE 会话。关闭连接即取消；命中后的「绑定 + 钉住」由服务端独立完成。 */
export function useAccountTurnStateHunt() {
  const status = ref<TurnStateHuntStatus>('idle')
  const rows = ref<TurnStateHuntRow[]>([])
  const expectedLength = ref<number | null>(null)
  const requests = ref(0)
  const message = ref('')
  /** 命中后账号绑定是否发生了变化；用于提示调用方刷新账号列表。 */
  const boundChanged = ref(false)
  const source = shallowRef<EventSource | null>(null)

  function close() {
    source.value?.close()
    source.value = null
  }

  function row(proxyId: string | null) {
    return rows.value.find(item => item.key === rowKey(proxyId))
  }

  function handle(event: TurnStateHuntEvent) {
    switch (event.type) {
      case 'hunt_start':
        expectedLength.value = event.expectedLength
        rows.value = event.proxies.map(proxy => ({
          ...proxy,
          key: rowKey(proxy.proxyId),
          state: 'pending',
          skipped: null,
          attempts: [],
        }))
        break
      case 'proxy_start': {
        const current = row(event.proxyId)
        if (current)
          current.state = 'running'
        break
      }
      case 'attempt':
        // 这里数的是探测尝试；其中在本地就失败的并没有发往上游，准确的请求数以结束事件为准。
        requests.value += 1
        row(event.proxyId)?.attempts.push({
          index: event.index,
          length: event.length,
          matched: event.matched,
          error: event.error,
        })
        break
      case 'proxy_done': {
        const current = row(event.proxyId)
        if (current) {
          current.skipped = event.skipped
          current.state = event.matched ? 'hit' : event.skipped ? 'skipped' : 'miss'
        }
        break
      }
      case 'hit':
        // 此后服务端不再受页面断开影响，界面也不再允许取消。
        status.value = 'finalizing'
        break
      case 'bound':
        boundChanged.value = event.changed
        break
      case 'pinned':
        message.value = `已钉住 ${event.model} 的 ${event.length} 字节 state，${new Date(event.expiresAt).toLocaleString()} 到期`
        break
      case 'hunt_complete':
        requests.value = event.requests
        status.value = event.success ? 'success' : 'miss'
        if (!event.success)
          message.value = '所有出口都没有返回符合规则的 state，账号设置未改动'
        close()
        break
      case 'error':
        status.value = 'error'
        message.value = event.message
        close()
        break
    }
  }

  function start(params: TurnStateHuntParam) {
    close()
    status.value = 'running'
    rows.value = []
    expectedLength.value = null
    requests.value = 0
    message.value = ''
    boundChanged.value = false
    const stream = new EventSource(turnStateHuntStreamUrl(params), { withCredentials: true })
    source.value = stream
    stream.onmessage = (raw) => {
      try {
        handle(JSON.parse(raw.data) as TurnStateHuntEvent)
      }
      catch {
        // 保活帧或无法解析的帧不影响遍历。
      }
    }
    stream.onerror = () => {
      // 服务端在参数或前置条件不满足时直接拒绝；EventSource 读不到响应正文。
      if (source.value !== stream)
        return
      close()
      if (status.value === 'running' || status.value === 'finalizing') {
        const started = rows.value.length > 0
        status.value = 'error'
        message.value = started
          ? '连接中断；若已命中，绑定与钉住仍会在服务端完成，请重新打开账号查看'
          : '无法开始遍历：请确认已开启并保存「固定自身 state」、至少有一个测试通过的代理，且该账号没有其它遍历在运行'
      }
    }
  }

  function cancel() {
    if (status.value !== 'running')
      return
    close()
    status.value = 'cancelled'
    message.value = '已取消。若取消的瞬间恰好命中，服务端可能仍在完成绑定与钉住；稍后会按服务端的实际状态刷新下方的代理与固定列表，请以刷新后的内容为准（可重新打开账号再次确认）'
  }

  onScopeDispose(close)

  return { status, rows, expectedLength, requests, message, boundChanged, start, cancel }
}
