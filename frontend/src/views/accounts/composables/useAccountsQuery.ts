import type { BaseTableSort } from '@/components/base/BaseTable/columns'
import { useDocumentVisibility, useIntervalFn, useStorage, watchDebounced } from '@vueuse/core'

import { computed, onMounted, shallowRef, watch } from 'vue'
import { getAccounts } from '@/api'
import { usePagedQuery } from '@/composables/usePagedQuery'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]

/** 自动刷新可选间隔（秒）；0 表示关闭。状态、并发、报错与用量随线上流量变化。 */
export const AUTO_REFRESH_SECONDS = [0, 10, 15, 30, 60] as const
// 列表每次刷新都要对请求表做 24h/额度窗口聚合，默认档放宽到 30 s 降低数据库压力。
const DEFAULT_AUTO_REFRESH_SECONDS = 30

export function useAccountsQuery() {
  const searchQuery = shallowRef('')
  const providerQuery = shallowRef('')
  const statusQuery = shallowRef('')
  const groupQuery = shallowRef('')
  const sort = shallowRef<BaseTableSort>()
  const accountSummary = shallowRef({
    total: 0,
    normal: 0,
    quotaExhausted: 0,
    rateLimited: 0,
    disabled: 0,
    error: 0,
  })

  const query = usePagedQuery({
    initialPageSize: 20,
    load: ({ page, pageSize }, options) =>
      getAccounts({
        page,
        pageSize,
        search: searchQuery.value,
        provider: providerQuery.value || undefined,
        status: statusQuery.value || undefined,
        groupId: groupQuery.value || undefined,
        sortBy: sort.value?.key,
        sortDirection: sort.value?.direction,
      }, options),
    onSuccess: (result) => {
      accountSummary.value = result.summary
    },
  })

  const accountPagination = computed(() => ({
    currentPage: query.page.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))

  function handlePageChange(page: number) {
    query.page.value = page
    void query.execute()
  }

  function handlePageSizeChange(pageSize: number) {
    query.pageSize.value = pageSize
    query.page.value = 1
    void query.execute()
  }

  function handleSortChange(nextSort: BaseTableSort | undefined) {
    sort.value = nextSort
    query.page.value = 1
    void query.execute()
  }

  async function replaceAccount(updated: AccountRow) {
    // 先取消旧查询并应用接口返回的账号，避免旧响应覆盖最新行数据。
    query.invalidate()
    query.items.value = query.items.value.map(account => account.id === updated.id ? updated : account)

    // 筛选、排序、概览和末页回退仍由回读校准，但不触发整表加载。
    if (!await query.execute({ background: true }))
      return true // 回读失败或被新查询取代时，不依据旧页面取消选择。
    return query.items.value.some(account => account.id === updated.id)
  }

  watchDebounced(
    searchQuery,
    () => {
      query.page.value = 1
      void query.execute()
    },
    { debounce: 250 },
  )

  watch([providerQuery, statusQuery, groupQuery], () => {
    query.page.value = 1
    void query.execute()
  })

  onMounted(() => {
    void query.execute()
  })

  // 间隔按浏览器记忆；非法值回退默认，避免被改坏的存储导致高频请求。
  const autoRefreshSeconds = useStorage<number>(
    'codex-proxy:accounts:auto-refresh-seconds',
    DEFAULT_AUTO_REFRESH_SECONDS,
    undefined,
    { writeDefaults: false },
  )
  const autoRefreshInterval = computed(() => {
    const seconds = AUTO_REFRESH_SECONDS.find(value => value === autoRefreshSeconds.value)
      ?? DEFAULT_AUTO_REFRESH_SECONDS
    return seconds * 1000
  })
  const refreshing = shallowRef(false)

  // 后台标签页不刷新；上一次请求未返回时跳过本轮，避免请求堆积。
  const visibility = useDocumentVisibility()
  async function refreshInBackground() {
    if (visibility.value !== 'visible' || query.loading.value || refreshing.value)
      return
    refreshing.value = true
    try {
      await query.execute({ silent: true })
    }
    finally {
      refreshing.value = false
    }
  }
  const autoRefresh = useIntervalFn(() => void refreshInBackground(), () => autoRefreshInterval.value || 60_000, {
    immediate: false,
  })
  watch(autoRefreshInterval, (interval) => {
    if (interval > 0)
      autoRefresh.resume()
    else
      autoRefresh.pause()
  }, { immediate: true })
  watch(visibility, (current, previous) => {
    if (current === 'visible' && previous === 'hidden' && autoRefreshInterval.value > 0)
      void refreshInBackground()
  })

  return {
    page: query.page,
    pageSize: query.pageSize,
    totalAccounts: query.total,
    loading: query.loading,
    accounts: query.items,
    loadAccounts: query.execute,
    refreshAccountsSilently: () => query.execute({ silent: true }),
    autoRefreshSeconds,
    refreshing,
    refreshNow: refreshInBackground,
    searchQuery,
    providerQuery,
    statusQuery,
    groupQuery,
    sort,
    accountSummary,
    accountPagination,
    replaceAccount,
    handlePageChange,
    handlePageSizeChange,
    handleSortChange,
  }
}
