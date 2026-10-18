<script setup lang="ts">
import type { Account } from '@/api'
import { ZDialog, ZIconButton } from '@codex-proxy/ui'
import { RefreshCw } from '@lucide/vue'
import { toRef } from 'vue'
import { useAccountConcurrency } from '../composables/useAccountConcurrency'
import AccountConcurrencyTable from './AccountConcurrencyTable.vue'

const props = defineProps<{ account: Account | null }>()
const open = defineModel<boolean>({ required: true })
const { data, entries, loading, error, refresh } = useAccountConcurrency(toRef(props, 'account'), open)
</script>

<template>
  <ZDialog v-model="open" title="并发明细" description="当前正在处理的请求" width="72rem">
    <div class="grid min-w-0 gap-2">
      <div class="flex items-center justify-between gap-3">
        <div class="flex items-center text-cp-sm" role="group" aria-label="当前占用 / 并发上限">
          <span class="whitespace-nowrap font-mono tabular-nums">
            <strong class="font-emphasis text-cp-text">{{ data?.total ?? account?.capacity.usedSlots ?? '—' }}</strong>
            <span class="text-cp-text-tertiary"> / {{ account?.capacity.totalSlots ?? '不限' }}</span>
          </span>
        </div>
        <ZIconButton
          aria-label="刷新并发明细" title="每 5 秒自动刷新" variant="plain" size="small"
          :disabled="loading" :aria-busy="loading" @click="refresh"
        >
          <RefreshCw class="size-4" :class="{ 'animate-spin motion-reduce:animate-none': loading }" />
        </ZIconButton>
      </div>
      <AccountConcurrencyTable
        :entries="entries"
        :loading="loading && !data" :aria-busy="loading"
        :empty-text="error ? '暂时无法读取并发明细' : '当前没有并发占用'"
      />
    </div>
  </ZDialog>
</template>
