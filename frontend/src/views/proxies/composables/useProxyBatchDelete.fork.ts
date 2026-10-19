// fork: proxy-batch-delete — 上游只有单条删除；批量删除走 fork 后端 /api/admin/proxies/batch-delete。
import type { Ref } from 'vue'
import type { OutboundProxyRecord } from '@/api'
import { ZNotification } from '@codex-proxy/ui'
import { shallowRef } from 'vue'
import { batchDeleteProxies } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'

export function useProxyBatchDelete(options: {
  proxies: Ref<OutboundProxyRecord[]>
  selectedIds: Ref<Set<string>>
  reload: () => Promise<unknown>
}) {
  const showBatchDelete = shallowRef(false)
  const action = useAsyncAction()
  const batchDeleting = action.loading

  async function confirmBatchDelete() {
    if (batchDeleting.value)
      return
    const items = options.proxies.value
      .filter(proxy => options.selectedIds.value.has(proxy.id))
      .map(proxy => ({ id: proxy.id, revision: proxy.revision }))
    if (items.length === 0) {
      showBatchDelete.value = false
      return
    }
    await action.run(async () => {
      const result = await batchDeleteProxies({ items })
      showBatchDelete.value = false
      options.selectedIds.value = new Set()
      await options.reload()
      if (result.skipped.length > 0)
        ZNotification.warning({ message: `已删除 ${result.deletedIds.length} 条，跳过 ${result.skipped.length} 条（仍有账号使用或已被修改）` })
      else
        ZNotification.success({ message: `已删除 ${result.deletedIds.length} 条代理` })
    })
  }

  return { showBatchDelete, batchDeleting, confirmBatchDelete }
}
