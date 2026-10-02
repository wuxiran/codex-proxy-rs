<script setup lang="ts">
import type { TurnStateHistoryRow } from '../utils/turnStateHistory'
import type { TurnStateCaptureRule } from '@/api'
import { RefreshCw } from '@lucide/vue'
import { computed, ref, watch } from 'vue'
import { getUsageRecordDetail, getUsageRecords } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { useRequestState } from '@/composables/useRequestState'
import { turnStateHistoryRow } from '../utils/turnStateHistory'

const props = defineProps<{ accountId: string, captureRule: TurnStateCaptureRule | null }>()
const emit = defineEmits<{ close: [] }>()
const rows = ref<TurnStateHistoryRow[]>([])
const request = useRequestState()
const { loading, error } = request
const latestLength = computed(() => rows.value.find(row => row.lengths.length)?.lengths.join(' / '))
const columns = defineTableColumns<TurnStateHistoryRow>([
  { key: 'time', label: '请求时间', kind: 'datetime', size: 'lg' },
  { key: 'model', label: '模型', kind: 'text', size: 'md' },
  { key: 'lengths', label: 'state 长度', kind: 'custom', size: 'sm' },
  { key: 'result', label: '请求结果', kind: 'status', size: 'sm', align: 'left' },
  { key: 'reason', label: '筛选说明', kind: 'custom', size: '2xl' },
])

async function load() {
  const accountId = props.accountId
  const requestId = request.start()
  const signal = request.signal
  rows.value = []
  try {
    const end = new Date()
    const page = await getUsageRecords({
      currentPage: 1,
      pageSize: 10,
      accountId,
      provider: 'openai',
      startTime: new Date(end.getTime() - 24 * 60 * 60 * 1000).toISOString(),
      endTime: end.toISOString(),
    }, { signal, silent: true })
    if (!request.isCurrent(requestId))
      return
    const records = page.items.filter(record => record.accountId === accountId).slice(0, 10)
    // 上限十条、每批三项，避免一次点击产生无界的诊断查询。
    for (let offset = 0; offset < records.length; offset += 3) {
      const batch = records.slice(offset, offset + 3)
      const details = await Promise.allSettled(batch.map(record => getUsageRecordDetail({ id: record.id }, { signal, silent: true })))
      if (!request.isCurrent(requestId))
        return
      rows.value.push(...details.map((result, index) => turnStateHistoryRow(
        batch[index]!,
        result.status === 'fulfilled' ? result.value : null,
        accountId,
        props.captureRule,
      )))
    }
  }
  catch (cause) {
    request.fail(requestId, cause)
  }
  finally {
    request.finish(requestId)
  }
}

watch([() => props.accountId, () => props.captureRule], () => {
  void load()
}, { immediate: true })
</script>

<template>
  <section class="min-w-0 rounded-cp bg-cp-bg-container p-3" aria-label="最近捕获记录">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <h4 class="m-0 text-cp-sm font-heavy text-cp-text">
        最近捕获
      </h4>
      <div class="flex gap-2">
        <BaseButton variant="soft" size="sm" :loading="loading" @click="load">
          <template #icon>
            <RefreshCw :size="14" />
          </template>
          刷新记录
        </BaseButton>
        <BaseButton variant="ghost" size="sm" @click="emit('close')">
          收起
        </BaseButton>
      </div>
    </div>
    <p class="mt-2 mb-1 text-cp-xs leading-relaxed text-cp-text-secondary">
      最近 24 小时最多 10 条可查询的用量记录，包含未满足固定条件的返回值。这里只显示诊断摘要，当前已固定项仍以上方列表为准。
    </p>
    <p v-if="latestLength" class="my-2 text-cp-sm font-emphasis text-cp-primary-text" role="status">
      最近可见的 state 长度：{{ latestLength }} 字节
    </p>
    <p v-if="error" class="my-3 text-cp-sm text-cp-error-text" role="alert">
      {{ error }}
    </p>
    <p v-else-if="loading" class="my-3 text-cp-sm text-cp-text-secondary" role="status">
      正在读取最近捕获记录…
    </p>
    <BaseEmpty v-else-if="!rows.length" title="最近 24 小时暂无请求记录" description="该账号有新请求后，可点击刷新记录查看。" size="sm" surface="none" />
    <div v-if="rows.length" class="mt-2 h-64 min-w-0">
      <BaseTable :columns="columns" :rows="rows" density="compact" class="h-full" empty-text="暂无捕获记录">
        <template #lengths="{ row }">
          <span class="font-mono font-heavy" :class="row.matchesRule ? 'text-cp-primary-text' : 'text-cp-text'">{{ row.lengths.join(' / ') || '—' }}</span>
        </template>
        <template #reason="{ row }">
          <div class="whitespace-normal text-cp-xs leading-relaxed">
            {{ row.reason }}
            <span v-if="row.partial" class="block text-cp-text-secondary">诊断记录不完整</span>
          </div>
        </template>
      </BaseTable>
    </div>
  </section>
</template>
