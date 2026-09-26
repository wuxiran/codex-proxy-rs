<script setup lang="ts">
import type { AccountTicket } from '@/api/modules/accounts'
import { computed } from 'vue'

import { useUiClock } from '@/composables/useUiClock'
import { formatDateTime } from '@/utils/date'

const props = defineProps<{ ticket: AccountTicket, addedAtDisplay?: string | null }>()

/** 剩余不足此时长时标红提醒。 */
const WARN_MS = 2 * 3600 * 1000

const now = useUiClock()

/** 票据是否已过期：过期即视作「无票据」状态，不再展示到期行与「已存票据」提示。 */
const isExpired = computed(() => {
  const at = props.ticket.expiresAt ? Date.parse(props.ticket.expiresAt) : Number.NaN
  return !Number.isNaN(at) && at - now.value.getTime() <= 0
})

/** 未过期时的到期倒计时；已过期返回 null（隐藏）。 */
const expiry = computed(() => {
  const at = props.ticket.expiresAt ? Date.parse(props.ticket.expiresAt) : Number.NaN
  if (Number.isNaN(at))
    return null
  const remaining = at - now.value.getTime()
  if (remaining <= 0)
    return null
  const time = formatDateTime(props.ticket.expiresAt, '—').slice(5, 16)
  const hours = Math.floor(remaining / 3600_000)
  const minutes = Math.floor((remaining % 3600_000) / 60_000)
  const left = hours >= 48 ? `${Math.floor(hours / 24)} 天` : hours > 0 ? `${hours} 小时 ${minutes} 分` : `${minutes} 分`
  return {
    text: `${time} 到期（剩 ${left}）`,
    tone: remaining < WARN_MS ? 'text-cp-error-text' : 'text-cp-text-secondary',
  }
})

/** 过期后仍保留「自动复活进行中/已停止」的告警行（有意义的活信号）；空闲的「已存票据」提示则随过期隐藏。 */
const showTicketLine = computed(() => props.ticket.hasTicket && (!isExpired.value || props.ticket.autoReviveAttempts > 0))

/** 加入时间取「月-日 时:分」，去掉年份省空间。 */
const addedText = computed(() => {
  const d = props.addedAtDisplay
  if (!d)
    return null
  const trimmed = d.length >= 16 && /^\d{4}-/.test(d) ? d.slice(5, 16) : d
  return `加入 ${trimmed}`
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
    <span
      v-if="showTicketLine"
      class="whitespace-nowrap text-[10px]"
      :class="ticket.autoReviveAttempts >= 3 ? 'text-cp-error-text' : 'text-cp-text-tertiary'"
      :title="ticket.autoReviveLastError
        ? `上次自动复活失败（${ticket.autoReviveLastAt ? formatDateTime(ticket.autoReviveLastAt) : ''}）：${ticket.autoReviveLastError}`
        : (ticket.ticketHint ?? undefined)"
    >
      {{ ticket.autoReviveAttempts > 0
        ? `自动复活 ${ticket.autoReviveAttempts}/3${ticket.autoReviveAttempts >= 3 ? '，已停止' : ''}`
        : '已存票据 · 自动复活' }}
    </span>
    <span
      v-if="addedText"
      class="whitespace-nowrap text-[10px] text-cp-text-quaternary"
      :title="addedAtDisplay ? `加入于 ${addedAtDisplay}` : undefined"
    >
      {{ addedText }}
    </span>
    <span v-if="!ticket.purchaseDisplay && !expiry && !showTicketLine && !addedText" class="text-cp-text-quaternary">—</span>
  </div>
</template>
