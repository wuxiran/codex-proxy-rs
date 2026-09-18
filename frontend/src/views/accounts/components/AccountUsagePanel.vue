<script setup lang="ts">
import type { AccountRow } from '../constants'

import { ChartNoAxesCombined, Sigma } from '@lucide/vue'
import { computed, ref } from 'vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { modelSuccessRateTextClass } from '../constants'
import AccountQuotaForecastModal from './AccountQuotaForecastModal/index.vue'

const props = defineProps<{
  account: AccountRow
}>()

const emit = defineEmits<{
  accountUpdated: [account: AccountRow]
}>()
const forecastOpen = ref(false)

type AccountModelUsage = AccountRow['usage']['models'][number]

const totalBillingDisplay = computed(() => props.account.usage.billing?.modelPriceAmountUsdDisplay ?? '未提供')
const hasUsageSummary = computed(() => (props.account.usage.requestCount ?? 0) > 0)

const modelUsageColumns = defineTableColumns<AccountModelUsage>([
  { key: 'requestedModelId', label: '请求模型', kind: 'text', size: 'md' },
  { key: 'upstreamModelId', label: '路由 / 响应模型', kind: 'text', size: 'lg' },
  { key: 'billingModel', label: '计费模型', kind: 'text', size: 'md' },
  { key: 'billing.modelPriceAmountUsdDisplay', format: (_value, row) => row.billing?.modelPriceAmountUsdDisplay ?? '未提供', label: '按模型价格计费', kind: 'numeric', size: 'md' },
  { key: 'billing.upstreamCostAmountUsdDisplay', format: (_value, row) => row.billing?.upstreamCostAmountUsdDisplay ?? '未提供', label: '真实上游费用', kind: 'numeric', size: 'lg' },
  { key: 'billing.differenceAmountUsdDisplay', format: (_value, row) => row.billing?.differenceAmountUsdDisplay ?? '不可计算', label: '差额', kind: 'numeric', size: 'sm' },
  { key: 'requestCountDisplay', label: '调用 / 成功率', kind: 'numeric', size: 'sm' },
])
</script>

<template>
  <section class="grid min-w-0 gap-4 rounded-lg bg-cp-bg-container p-4 shadow-cp-tertiary">
    <div class="min-w-0">
      <div class="mb-3 flex shrink-0 items-baseline justify-between gap-3">
        <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
          Tokens 结构
        </h3>
        <span class="text-cp-xs font-emphasis text-cp-text-quaternary">{{ account.usage.windowLabelDisplay }}</span>
      </div>
      <div class="grid grid-cols-2 gap-2 sm:grid-cols-5">
        <div class="flex items-center justify-between rounded-lg bg-cp-green-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-green-on-container">输入</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.inputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-orange-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-orange-on-container">输出</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.outputTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-cyan-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-cyan-on-container">缓存</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.cachedTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-blue-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-blue-on-container">推理</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.reasoningTokensDisplay }}
          </strong>
        </div>
        <div class="flex items-center justify-between rounded-lg bg-cp-blue-container px-3 py-2">
          <span class="text-cp-sm font-bold text-cp-blue-on-container">读取</span>
          <strong class="font-mono text-cp text-cp-text">
            {{ account.usage.readTokensDisplay }}
          </strong>
        </div>
      </div>
    </div>

    <div class="min-w-0">
      <div class="mb-3 flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2">
        <div class="flex shrink-0 items-center gap-1">
          <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
            模型使用排行
          </h3>
          <BaseIconButton
            v-if="account.authenticationKind !== 'api_key'"
            label="预测周/月额度"
            size="sm"
            aria-haspopup="dialog"
            @click="forecastOpen = true"
          >
            <ChartNoAxesCombined class="size-3.5" :stroke-width="1.75" />
          </BaseIconButton>
        </div>

        <div class="ml-auto flex flex-wrap items-baseline gap-2">
          <div v-if="hasUsageSummary" class="flex items-baseline gap-1.5 whitespace-nowrap">
            <Sigma class="size-3.5 self-center text-cp-text-tertiary" :stroke-width="1.75" />
            <span title="总 Token">
              <span class="sr-only">总 Token：</span>
              <span class="font-mono text-cp-sm font-emphasis tabular-nums text-cp-text">
                {{ account.usage.totalTokensDisplay }}
              </span>
            </span>
            <span class="mx-0.5 text-[10px] leading-none font-emphasis text-cp-text-quaternary"> / </span>
            <span title="按模型价格计费">
              <span class="text-cp-xs text-cp-text-secondary">按模型价格计费：</span>
              <span class="font-mono text-cp-sm font-heavy tabular-nums text-cp-green-text">
                {{ totalBillingDisplay }}
              </span>
            </span>
          </div>
          <span class="whitespace-nowrap text-cp-xs font-emphasis text-cp-text-quaternary">{{ account.usage.windowLabelDisplay }}</span>
        </div>
      </div>

      <p class="mb-2 whitespace-normal text-cp-xs text-cp-text-tertiary">
        差额 = 按模型价格计费 − 真实上游费用；仅同一行全部请求的两项 USD 金额均已提供时计算。历史未记录的模型显示“未记录”。
      </p>
      <div class="h-64 min-w-0">
        <BaseTable
          :columns="modelUsageColumns"
          :rows="account.usage.models"
          row-key="key"
          density="compact"
          empty-text="暂无模型用量"
        >
          <template #requestedModelId="{ row }">
            <div class="grid gap-1">
              <span>{{ row.requestedModelId ?? '未记录' }}</span>
              <span class="text-[10px] text-cp-text-tertiary" :title="`输入 ${row.inputTokensDisplay} · 输出 ${row.outputTokensDisplay} · 缓存 ${row.cachedTokensDisplay}`">Tokens {{ row.totalTokensDisplay }}</span>
              <span v-if="row.mismatch" class="w-fit rounded-cp bg-cp-warning-container px-1.5 text-cp-xs font-bold text-cp-warning-on-container">mismatch</span>
            </div>
          </template>
          <template #upstreamModelId="{ row }">
            <div class="grid gap-1 text-cp-xs">
              <span>路由：{{ row.upstreamModelId ?? '未记录' }}</span>
              <span>响应：{{ row.responseModel ?? '未记录' }}</span>
            </div>
          </template>
          <template #billingModel="{ row }">
            <div class="grid gap-1">
              <span>{{ row.billingModel ?? '未记录' }}</span>
              <span class="text-[10px] text-cp-text-tertiary" :title="row.lastUsedAt">{{ row.lastUsedAtDisplay }}</span>
            </div>
          </template>
          <template #requestCountDisplay="{ row }">
            <div class="grid gap-1">
              <span>{{ row.requestCountDisplay }}</span>
              <span :class="modelSuccessRateTextClass(row.successRate)">{{ row.successRateDisplay }}</span>
            </div>
          </template>
        </BaseTable>
      </div>
    </div>
  </section>

  <AccountQuotaForecastModal
    v-if="account.authenticationKind !== 'api_key'"
    v-model="forecastOpen"
    :account="account"
    @account-updated="emit('accountUpdated', $event)"
  />
</template>
