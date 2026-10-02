<script setup lang="ts">
import type { OpsError } from '@/api/modules/usage'
import { computed, shallowRef, useId } from 'vue'

import { getOpsErrors } from '@/api'
import BasePopover from '@/components/base/BasePopover.vue'
import { errorMessage } from '@/utils/async'
import { errorRateDisplay } from '../constants'

const props = defineProps<{
  accountId: string
  requestCount: number
  errorCount: number
}>()

const RECENT_LIMIT = 5
/** 同一账号短时间内反复悬停不重复请求。 */
const CACHE_MS = 20_000

const detailId = `account-recent-errors-${useId()}`
const open = shallowRef(false)
const loading = shallowRef(false)
const loadError = shallowRef('')
const items = shallowRef<OpsError[]>([])
const total = shallowRef(0)
let loadedAt = 0
let requestSeq = 0

const hasErrors = computed(() => props.errorCount > 0)

async function load() {
  if (Date.now() - loadedAt < CACHE_MS)
    return
  const seq = ++requestSeq
  loading.value = true
  loadError.value = ''
  const end = new Date()
  const start = new Date(end.getTime() - 24 * 3600 * 1000)
  try {
    const result = await getOpsErrors({
      accountId: props.accountId,
      startTime: start.toISOString(),
      endTime: end.toISOString(),
      currentPage: 1,
      pageSize: RECENT_LIMIT,
    }, { silent: true })
    if (seq !== requestSeq)
      return
    items.value = result.items
    total.value = result.total
    loadedAt = Date.now()
  }
  catch (error: unknown) {
    if (seq === requestSeq)
      loadError.value = errorMessage(error, '读取失败')
  }
  finally {
    if (seq === requestSeq)
      loading.value = false
  }
}

function updateOpen(nextOpen: boolean) {
  // 没有报错时无内容可看，不弹出。
  open.value = nextOpen && hasErrors.value
  if (open.value)
    void load()
}

function errorCode(item: OpsError) {
  return item.providerErrorCode
    ?? (item.upstreamStatusCode === null ? null : String(item.upstreamStatusCode))
    ?? item.failureClass
}
</script>

<template>
  <BasePopover
    :model-value="open"
    trigger="hover-click"
    placement="right"
    :offset="12"
    :hover-delay="240"
    @update:model-value="updateOpen"
  >
    <template #trigger>
      <button
        type="button"
        class="whitespace-nowrap rounded-cp-sm border-0 bg-transparent p-0 text-left text-cp-xs font-emphasis tabular-nums outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
        :class="hasErrors ? 'cursor-help text-cp-error-text' : 'cursor-default text-cp-text-tertiary'"
        :aria-label="`最近 24 小时报错 ${errorCount} 次，查看最近报错`"
        aria-haspopup="dialog"
        :aria-expanded="open"
        :aria-controls="open ? detailId : undefined"
      >
        24h 报错 {{ errorCount }}{{ errorRateDisplay({ requestCount, errorCount }) }}
      </button>
    </template>

    <section
      :id="detailId"
      class="w-[min(26rem,calc(100vw-1rem))] overflow-hidden rounded-cp-lg"
      role="dialog"
      aria-label="最近报错"
    >
      <header class="flex items-baseline justify-between gap-3 border-b border-cp-split px-3 py-2">
        <span class="text-cp-sm font-heavy text-cp-text">最近报错</span>
        <span class="text-cp-xs tabular-nums text-cp-text-tertiary">
          24h 共 {{ total || errorCount }} 条，已结束请求 {{ requestCount }} 次
        </span>
      </header>
      <p v-if="loading && items.length === 0" class="m-0 px-3 py-3 text-cp-sm text-cp-text-tertiary">
        加载中...
      </p>
      <p v-else-if="loadError" class="m-0 px-3 py-3 text-cp-sm text-cp-error-text">
        {{ loadError }}
      </p>
      <p v-else-if="items.length === 0" class="m-0 px-3 py-3 text-cp-sm text-cp-text-tertiary">
        暂无报错明细
      </p>
      <ol v-else class="m-0 list-none divide-y divide-cp-split p-0">
        <li v-for="item in items" :key="item.id" class="grid gap-0.5 px-3 py-2">
          <div class="flex items-center gap-2 text-cp-xs tabular-nums">
            <span class="text-cp-text-tertiary">{{ item.createdAtDisplay }}</span>
            <span v-if="item.model" class="text-cp-text-secondary">{{ item.model }}</span>
            <span class="ml-auto font-mono text-cp-error-text">{{ errorCode(item) }}</span>
          </div>
          <p class="m-0 line-clamp-2 text-cp-xs leading-4 text-cp-text wrap-anywhere select-text" :title="item.message">
            {{ item.message }}
          </p>
        </li>
      </ol>
    </section>
  </BasePopover>
</template>
