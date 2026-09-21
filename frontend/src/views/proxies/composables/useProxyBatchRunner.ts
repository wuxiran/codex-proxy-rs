import { computed, onScopeDispose, shallowRef } from 'vue'

export type ProxyBatchKind = 'test' | 'quality'

/**
 * 前端并发池：后端每进程只有 4 个测试槽位，批量并发必须低于它，
 * 给同时进行的单条测试留出余量；超出的请求会在后端短暂排队而不是失败。
 */
export function useProxyBatchRunner() {
  const kind = shallowRef<ProxyBatchKind | null>(null)
  const done = shallowRef(0)
  const total = shallowRef(0)
  const cancelled = shallowRef(false)

  const running = computed(() => kind.value !== null)
  const progressLabel = computed(() => `${done.value}/${total.value}`)

  async function run<Item>(
    batchKind: ProxyBatchKind,
    items: Item[],
    concurrency: number,
    worker: (item: Item) => Promise<void>,
  ): Promise<{ completed: number, cancelled: boolean }> {
    if (running.value || items.length === 0)
      return { completed: 0, cancelled: false }
    kind.value = batchKind
    done.value = 0
    total.value = items.length
    cancelled.value = false
    let cursor = 0
    try {
      await Promise.all(Array.from({ length: Math.min(concurrency, items.length) }, async () => {
        while (!cancelled.value && cursor < items.length) {
          const item = items[cursor]!
          cursor += 1
          // 单条失败已由 worker 自行记录；批量不因一条异常中断。
          await worker(item).catch(() => {})
          done.value += 1
        }
      }))
      return { completed: done.value, cancelled: cancelled.value }
    }
    finally {
      kind.value = null
    }
  }

  function cancel() {
    cancelled.value = true
  }

  onScopeDispose(cancel)

  return { kind, running, done, total, progressLabel, run, cancel }
}
