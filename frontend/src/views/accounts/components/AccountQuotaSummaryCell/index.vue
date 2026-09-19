<script setup lang="ts">
import type { AccountRow } from '../../constants'

import { computed } from 'vue'
import { groupedAccountQuotaWindows, visibleSummaryQuotaWindows } from '../../constants'
import AccountUsageWindow from '../AccountUsageWindow/index.vue'
import { quotaWindowPresentation, readUsdCost } from '../AccountUsageWindow/presenter'
import AccountQuotaSummaryEntry from './Entry.vue'
import { accountQuotaUsdSummary, recentlyUsedQuotaEntry, representativeQuotaWindow } from './presenter'

const props = defineProps<{
  account: AccountRow
}>()

const quotaWindows = computed(() => props.account.quota.windows)
const visibleQuotaWindows = computed(() => visibleSummaryQuotaWindows(quotaWindows.value))
const summaryEntries = computed(() => groupedAccountQuotaWindows(visibleQuotaWindows.value))
const hasUsage = computed(() => (props.account.usage.requestCount ?? 0) > 0)
const recentUsageEntry = computed(() => recentlyUsedQuotaEntry(
  summaryEntries.value,
  props.account.usage.models,
))
const currentUsageWindow = computed(() => representativeQuotaWindow(recentUsageEntry.value))
const currentUsageDisplay = computed(() => currentUsageWindow.value?.usedPercentDisplay ?? '—')
const currentUsageTextClass = computed(() => currentUsageWindow.value
  ? quotaWindowPresentation(currentUsageWindow.value, '2px').percentTextClass
  : 'text-cp-text-quaternary')
const additionalEntryCount = computed(() => Math.max(summaryEntries.value.length - 1, 0))
// 缺少上游额度窗口时仍展示本机已记录的金额；缺失金额与已知零金额分开处理。
const unobservedUsd = computed(() => readUsdCost(props.account.usage.costs))
const unobservedCostTitle = computed(() => unobservedUsd.value
  ? `${props.account.usage.windowLabelDisplay}：本机已用 ${unobservedUsd.value.display}，按本机用量记录估算，不含站外消耗`
  : '尚无可用的本机美元金额记录')
const usdSummary = computed(() => accountQuotaUsdSummary(props.account, currentUsageWindow.value))
const primaryUsageTitle = computed(() => {
  if (usdSummary.value)
    return usdSummary.value.title
  return `${props.account.usage.windowLabelDisplay}总 Token`
})
</script>

<template>
  <div class="box-border grid min-h-16.5 w-full min-w-0 content-center gap-1.5 py-1.5">
    <template v-if="account.authenticationKind === 'api_key'">
      <span
        class="flex min-w-0 items-baseline gap-1 font-mono tabular-nums"
        :title="usdSummary?.title ?? '本地累计总 Token'"
      >
        <strong class="truncate text-cp-xs font-heavy text-cp-text">
          {{ usdSummary?.usedDisplay ?? account.usage.totalTokensDisplay }}
        </strong>
        <span
          v-if="usdSummary?.estimatedQuotaDisplay"
          class="min-w-0 truncate text-[9px] font-emphasis text-cp-text-tertiary"
        >
          / ≈{{ usdSummary.estimatedQuotaDisplay }}
        </span>
        <span class="shrink-0 text-[9px] font-emphasis tracking-[0.02em] text-cp-text-quaternary">
          {{ usdSummary ? 'USD' : 'Tokens' }}
        </span>
      </span>
      <div class="grid min-w-0 gap-1.5">
        <span class="text-[10px] leading-3 font-bold text-cp-text-quaternary">{{ account.usage.windowLabelDisplay }}</span>
        <div class="h-1 w-full rounded-full bg-cp-success" title="上游额度未提供；绿色条不表示剩余额度" aria-hidden="true" />
      </div>
    </template>
    <template v-else-if="summaryEntries.length > 0">
      <div
        v-if="hasUsage"
        class="flex min-w-0 items-baseline justify-between gap-2 leading-none"
      >
        <span
          class="flex min-w-0 items-baseline gap-1 font-mono tabular-nums"
          :title="primaryUsageTitle"
        >
          <strong class="truncate text-cp-xs font-heavy text-cp-text">
            {{ usdSummary?.usedDisplay ?? account.usage.totalTokensDisplay }}
          </strong>
          <span
            v-if="usdSummary?.estimatedQuotaDisplay"
            class="min-w-0 truncate text-[9px] font-emphasis text-cp-text-tertiary"
          >
            / ≈{{ usdSummary.estimatedQuotaDisplay }}
          </span>
          <span class="shrink-0 text-[9px] font-emphasis tracking-[0.02em] text-cp-text-quaternary">
            {{ usdSummary ? 'USD' : 'Tokens' }}
          </span>
        </span>
        <span
          class="flex shrink-0 items-baseline gap-1 text-[9px] font-emphasis text-cp-text-quaternary"
          title="最近使用额度的当前已用比例"
        >
          <span>使用率</span>
          <strong class="font-mono font-heavy tabular-nums" :class="currentUsageTextClass">
            {{ currentUsageDisplay }}
          </strong>
        </span>
      </div>

      <div v-if="recentUsageEntry" class="flex min-w-0 items-end gap-2">
        <div class="flex min-w-0 flex-1">
          <AccountQuotaSummaryEntry
            :label="recentUsageEntry.label"
            :windows="recentUsageEntry.windows"
            :show-percentage="false"
          />
        </div>
        <span
          v-if="additionalEntryCount > 0"
          class="grid h-5 min-w-5 shrink-0 place-items-center rounded-cp bg-cp-fill-quaternary px-1.5 font-mono text-[9px] font-heavy tabular-nums text-cp-text-tertiary"
          :title="`另有 ${additionalEntryCount} 个额度组，可展开账号查看`"
        >
          +{{ additionalEntryCount }}
        </span>
      </div>
    </template>
    <template v-else>
      <div class="flex min-w-0 items-baseline gap-1 leading-none" :title="unobservedCostTitle">
        <span class="shrink-0 text-[9px] font-emphasis text-cp-text-quaternary">已用</span>
        <strong class="truncate font-mono text-cp-xs font-heavy tabular-nums text-cp-text">{{ unobservedUsd?.display ?? '—' }}</strong>
        <span v-if="unobservedUsd" class="shrink-0 text-[9px] font-emphasis text-cp-text-quaternary">USD</span>
      </div>
      <AccountUsageWindow variant="compact" />
    </template>
  </div>
</template>
