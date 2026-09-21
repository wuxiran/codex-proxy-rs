<script setup lang="ts">
import type { AccountRow } from '../../constants'
import { usePreferredReducedMotion } from '@vueuse/core'
import { computed, ref, toRef, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useChartPalette } from '@/composables/useChartPalette'
import { useUiClock } from '@/composables/useUiClock'
import { useAccountQuotaForecast } from '../../composables/useAccountQuotaForecast'
import { burnChartOption, formatDurationUntil } from './burnPresenter'

const props = defineProps<{
  account: AccountRow
  refreshing: boolean
}>()

const emit = defineEmits<{
  accountUpdated: [account: AccountRow]
}>()

// 面板只在展开行内挂载，挂载即视为打开；预测查询本身只读，不触发上游刷新。
const open = ref(true)
const { report, loading, error, load } = useAccountQuotaForecast(
  toRef(() => props.account.id),
  open,
  account => emit('accountUpdated', account),
)
const { palette } = useChartPalette()
const reducedMotion = usePreferredReducedMotion()
const now = useUiClock()

// 曲线与进度条同口径：优先周额度，没有周窗口时后端会给出折算来源。
const forecast = computed(() => report.value?.forecasts.find(item => item.period === 'weekly') ?? null)
const option = computed(() => forecast.value
  ? burnChartOption(forecast.value, palette.value, reducedMotion.value === 'reduce')
  : null)

const exhaustion = computed(() => {
  const value = forecast.value?.exhaustion
  if (!value && error.value && !forecast.value)
    return { text: '—', hint: null, tone: 'text-cp-text-tertiary' }
  if (!value)
    return { text: '样本不足', hint: forecast.value?.unavailableReason ?? null, tone: 'text-cp-text-tertiary' }
  if (value.kind === 'reached')
    return { text: '已耗尽', hint: null, tone: 'text-cp-error-text' }
  if (value.kind === 'afterReset')
    return { text: '重置前不会耗尽', hint: null, tone: 'text-cp-success-text' }
  return {
    text: value.atDisplay ?? '—',
    hint: formatDurationUntil(value.at, now.value.getTime()),
    tone: 'text-cp-warning-text',
  }
})

// 「刷新额度」由父级执行；结束后重新取样，让曲线末点跟上新的已用比例。
watch(() => props.refreshing, (refreshing, wasRefreshing) => {
  if (wasRefreshing && !refreshing)
    void load()
})
</script>

<template>
  <div class="mt-4 grid min-h-0 flex-1 content-start gap-3 border-t border-cp-split pt-3">
    <dl class="m-0 flex flex-wrap items-baseline gap-x-5 gap-y-1 text-cp-xs">
      <div class="flex items-baseline gap-1.5">
        <dt class="font-emphasis text-cp-text-secondary">
          加入时间
        </dt>
        <dd class="m-0 font-mono tabular-nums text-cp-text" :title="account.addedAt">
          {{ account.addedAtDisplay }}
        </dd>
      </div>
      <div class="flex min-w-0 items-baseline gap-1.5" :title="exhaustion.hint ?? undefined">
        <dt class="font-emphasis text-cp-text-secondary">
          预计耗尽
        </dt>
        <dd class="m-0 flex min-w-0 items-baseline gap-1.5">
          <BaseSkeleton v-if="loading && !forecast" shape="text" class="w-28 self-center" />
          <template v-else>
            <span class="font-mono font-heavy tabular-nums" :class="exhaustion.tone">{{ exhaustion.text }}</span>
            <span v-if="forecast?.exhaustion?.kind === 'at' && exhaustion.hint" class="text-cp-text-tertiary">
              {{ exhaustion.hint }}
            </span>
          </template>
        </dd>
      </div>
      <div v-if="forecast?.burnPercentPerHour != null" class="flex items-baseline gap-1.5">
        <dt class="font-emphasis text-cp-text-secondary">
          消耗速率
        </dt>
        <dd class="m-0 font-mono tabular-nums text-cp-text">
          {{ forecast.burnPercentPerHourDisplay }}
        </dd>
      </div>
    </dl>

    <div aria-live="polite" :aria-busy="loading">
      <BaseSkeleton v-if="loading && !forecast" class="h-28 w-full" />
      <div v-else-if="error && !forecast" class="grid h-28 place-items-center gap-2 text-center">
        <p class="m-0 text-cp-xs text-cp-text-tertiary">
          消耗曲线加载失败
        </p>
        <BaseButton variant="secondary" size="sm" @click="load">
          重新加载
        </BaseButton>
      </div>
      <figure v-else-if="option" class="m-0">
        <figcaption class="mb-1 flex items-baseline justify-between gap-2 text-cp-xs">
          <span class="font-emphasis text-cp-text-secondary">{{ forecast?.source?.label ?? '额度' }}消耗曲线</span>
          <span v-if="forecast?.lowSample" class="text-cp-text-quaternary">样本较少，仅供参考</span>
        </figcaption>
        <BaseChart :option="option" :height="112" />
      </figure>
      <p v-else class="m-0 grid h-28 place-items-center px-2 text-center text-cp-xs leading-relaxed text-cp-text-tertiary">
        {{ forecast?.unavailableReason ?? '本周期还没有额度观测记录，产生请求后会在这里画出消耗曲线。' }}
      </p>
    </div>
  </div>
</template>
