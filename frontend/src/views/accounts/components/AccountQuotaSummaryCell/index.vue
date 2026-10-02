<script setup lang="ts">
import type { AccountRow } from '../../constants'

import { computed } from 'vue'
import { groupedAccountQuotaWindows, visibleSummaryQuotaWindows } from '../../constants'
import AccountUsageWindow from '../AccountUsageWindow/index.vue'
import AccountQuotaSummaryEntry from './Entry.vue'
import { recentlyUsedQuotaEntry } from './presenter'

const props = defineProps<{ account: AccountRow }>()
const summaryEntries = computed(() => groupedAccountQuotaWindows(visibleSummaryQuotaWindows(props.account.quota.windows)))
const recentUsageEntry = computed(() => recentlyUsedQuotaEntry(summaryEntries.value, props.account.usage.models))
const additionalEntryCount = computed(() => Math.max(summaryEntries.value.length - 1, 0))
</script>

<template>
  <div class="box-border grid min-h-16.5 w-full min-w-0 content-center gap-1.5 py-1.5">
    <div class="grid gap-1" :title="account.usage.windowLabelDisplay">
      <span class="text-[10px] font-emphasis text-cp-text-secondary">按模型价格计费</span>
      <strong class="font-mono text-cp-xs font-heavy tabular-nums text-cp-text">{{ account.usage.billing?.modelPriceAmountUsdDisplay ?? '未提供' }}</strong>
      <span
        v-if="account.usage.estimatedQuotaUsdDisplay"
        class="text-[10px] text-cp-text-secondary"
        title="预估额度 ≈ 本窗口按模型价格计费 ÷ 额度已用比例；仅供参考，站外消耗与未计价请求会让它偏低"
      >
        预估额度：<span class="font-mono font-heavy tabular-nums text-cp-text">{{ account.usage.estimatedQuotaUsdDisplay }}</span>
      </span>
      <span class="text-[10px] text-cp-text-tertiary">
        真实上游费用：<span class="font-mono tabular-nums">{{ account.usage.billing?.upstreamCostAmountUsdDisplay ?? '未提供' }}</span>
      </span>
      <span class="text-[10px] text-cp-text-quaternary">{{ account.usage.windowLabelDisplay }}</span>
    </div>
    <div v-if="recentUsageEntry" class="flex min-w-0 items-end gap-2">
      <div class="flex min-w-0 flex-1">
        <AccountQuotaSummaryEntry :label="recentUsageEntry.label" :windows="recentUsageEntry.windows" />
      </div>
      <span v-if="additionalEntryCount > 0" class="text-[10px] text-cp-text-tertiary" :title="`另有 ${additionalEntryCount} 个额度组，可展开账号查看`">+{{ additionalEntryCount }}</span>
    </div>
    <AccountUsageWindow v-else-if="account.authenticationKind !== 'api_key'" variant="compact" />
  </div>
</template>
