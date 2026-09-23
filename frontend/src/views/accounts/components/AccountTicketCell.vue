<script setup lang="ts">
import type { AccountTicket } from '@/api/modules/accounts'
import { computed } from 'vue'

import { useUiClock } from '@/composables/useUiClock'
import { formatDateTime } from '@/utils/date'

const props = defineProps<{ ticket: AccountTicket }>()

/** 剩余不足此时长时标红提醒。 */
const WARN_MS = 2 * 3600 * 1000

const now = useUiClock()

const expiry = computed(() => {
  const at = props.ticket.expiresAt ? Date.parse(props.ticket.expiresAt) : Number.NaN
  if (Number.isNaN(at))
    return null
  const remaining = at - now.value.getTime()
  const time = formatDateTime(props.ticket.expiresAt, '—').slice(5, 16)
  if (remaining <= 0)
    return { text: `已过期（${time}）`, tone: 'text-cp-error-text' }
  const hours = Math.floor(remaining / 3600_000)
  const minutes = Math.floor((remaining % 3600_000) / 60_000)
  const left = hours >= 48 ? `${Math.floor(hours / 24)} 天` : hours > 0 ? `${hours} 小时 ${minutes} 分` : `${minutes} 分`
  return {
    text: `${time} 到期（剩 ${left}）`,
    tone: remaining < WARN_MS ? 'text-cp-error-text' : 'text-cp-text-secondary',
  }
})
</script>

<template>
  <div class="grid min-w-0 gap-0.5 text-cp-xs tabular-nums">
    <span v-if="ticket.purchaseDisplay" class="whitespace-nowrap" :title="ticket.purchasedAt ? `买入于 ${formatDateTime(ticket.purchasedAt)}` : undefined">
      <span class="font-heavy text-cp-text">{{ ticket.purchaseDisplay }}</span>
      <span class="text-cp-text-tertiary"> → 已刷 </span>
      <span class="font-mono font-heavy text-cp-text">{{ ticket.spentUsdDisplay ?? '$0.00' }}</span>
    </span>
    <span v-if="expiry" class="whitespace-nowrap font-emphasis" :class="expiry.tone">{{ expiry.text }}</span>
    <span v-if="ticket.hasTicket" class="whitespace-nowrap text-[10px] text-cp-text-tertiary" :title="ticket.ticketHint ?? undefined">
      已存票据
    </span>
    <span v-if="!ticket.purchaseDisplay && !expiry && !ticket.hasTicket" class="text-cp-text-quaternary">—</span>
  </div>
</template>
