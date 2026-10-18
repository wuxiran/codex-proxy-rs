// fork: proxy-quality — 单条 / 批量质量检测与报告弹窗状态。
// 批量测试连接走上游 useProxyBatchTesting；这里只负责上游没有的「上游可达性 / Cloudflare 挑战」检测。
import type { Ref } from 'vue'
import type { OutboundProxyRecord, ProxyQualityReport, ProxyQualityStatus } from '@/api'
import { ZNotification } from '@codex-proxy/ui'
import { computed, shallowRef } from 'vue'
import { checkProxyQuality } from '@/api'
import { useIdSet } from '@/composables/useIdSet'

export function useProxyQuality(options: { proxies: Ref<OutboundProxyRecord[]>, reload: () => Promise<unknown> }) {
  const checking = useIdSet<string>()
  const checkingIds = checking.ids
  const batchRunning = shallowRef(false)
  const batchDone = shallowRef(0)
  const batchTotal = shallowRef(0)
  const batchCancelled = shallowRef(false)
  const batchProgressLabel = computed(() => `${batchDone.value}/${batchTotal.value}`)

  const showReport = shallowRef(false)
  const reportProxy = shallowRef<OutboundProxyRecord | null>(null)
  const freshReport = shallowRef<ProxyQualityReport | null>(null)

  function openReport(proxy: OutboundProxyRecord, report: ProxyQualityReport | null = null) {
    reportProxy.value = proxy
    freshReport.value = report
    showReport.value = true
  }

  async function runQualityCheck(proxy: OutboundProxyRecord, silent: boolean) {
    if (checking.has(proxy.id))
      return null
    return checking.run(proxy.id, () => checkProxyQuality({ id: proxy.id, revision: proxy.revision }, { silent }))
  }

  async function checkQuality(proxy: OutboundProxyRecord) {
    openReport(proxy)
    try {
      const result = await runQualityCheck(proxy, false)
      if (!result)
        return
      // 弹窗可能已切到别的代理；只有仍在看这一条时才替换报告。
      if (reportProxy.value?.id === proxy.id) {
        reportProxy.value = result.record
        freshReport.value = result.report
      }
      ZNotification.success({ message: `${result.record.name}：评分 ${result.report.score}（${result.report.grade}）` })
    }
    // 失败提示由请求层给出；弹窗保持打开，继续展示上一次的报告（如果有）。
    catch {}
    finally {
      await options.reload()
    }
  }

  /** 批量检测当前页已勾选的代理；并发 2，与后端测试槽位留余量。 */
  async function batchQuality(ids: string[]) {
    if (batchRunning.value)
      return
    const wanted = new Set(ids)
    const items = options.proxies.value.filter(proxy => wanted.has(proxy.id))
    if (items.length === 0) {
      ZNotification.warning({ message: '所选代理已不存在，请刷新列表' })
      return
    }
    batchRunning.value = true
    batchCancelled.value = false
    batchDone.value = 0
    batchTotal.value = items.length
    const counts: Record<ProxyQualityStatus | 'error', number> = { healthy: 0, warn: 0, challenge: 0, failed: 0, error: 0 }
    let cursor = 0
    try {
      await Promise.all(Array.from({ length: Math.min(2, items.length) }, async () => {
        while (!batchCancelled.value && cursor < items.length) {
          const proxy = items[cursor]!
          cursor += 1
          try {
            const result = await runQualityCheck(proxy, true)
            if (result)
              counts[result.report.status] += 1
          }
          catch {
            counts.error += 1
          }
          batchDone.value += 1
        }
      }))
      const abnormal = counts.failed + counts.error
      const notify = counts.challenge + abnormal > 0 ? ZNotification.warning : ZNotification.success
      notify({ message: `${batchCancelled.value ? '已取消，' : ''}共检测 ${batchDone.value} 条：优质 ${counts.healthy}，告警 ${counts.warn}，挑战 ${counts.challenge}，异常 ${abnormal}` })
    }
    finally {
      batchRunning.value = false
      await options.reload()
    }
  }

  function cancelBatch() {
    batchCancelled.value = true
  }

  return {
    checkingIds,
    batchRunning,
    batchProgressLabel,
    showReport,
    reportProxy,
    freshReport,
    openReport,
    checkQuality,
    batchQuality,
    cancelBatch,
  }
}
