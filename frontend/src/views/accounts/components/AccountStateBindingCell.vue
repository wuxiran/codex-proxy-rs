<script setup lang="ts">
import type { OAuthStateConfiguration } from '@/api/modules/accounts'
import { computed } from 'vue'
import { useUiClock } from '@/composables/useUiClock'
import { formatDateTime } from '@/utils/date'

const props = defineProps<{
  configuration?: OAuthStateConfiguration
  loading?: boolean
  error?: string
}>()
const emit = defineEmits<{ configure: [], retry: [] }>()
const now = useUiClock()
const pins = computed(() => props.configuration?.turnStatePins ?? [])
const activePins = computed(() => pins.value.filter(pin => Date.parse(pin.expiresAt) > now.value.getTime()))
const nextExpiry = computed(() => [...activePins.value].sort((a, b) => Date.parse(a.expiresAt) - Date.parse(b.expiresAt))[0]?.expiresAt)
const label = computed(() => {
  if (!props.configuration)
    return props.loading ? '读取中...' : '读取失败'
  if (!props.configuration.pinTurnState)
    return '未开启'
  if (activePins.value.length)
    return `已绑定 ${activePins.value.length} 项`
  return pins.value.length ? '绑定已过期' : '等待绑定'
})
const title = computed(() => pins.value.map(pin => `${pin.model}：${formatDateTime(pin.expiresAt)} 到期`).join('\n'))
</script>

<template>
  <button
    v-if="error"
    type="button"
    class="cursor-pointer border-0 bg-transparent p-0 text-cp-sm text-cp-error"
    :title="error"
    @click="emit('retry')"
  >
    读取失败，重试
  </button>
  <button
    v-else
    type="button"
    class="flex min-h-10 w-full cursor-pointer flex-col items-start justify-center gap-1 border-0 bg-transparent p-0 text-left text-cp-sm text-cp-link hover:underline focus-visible:outline-2 focus-visible:outline-cp-control-outline"
    :title="title || '配置 state 绑定'"
    aria-label="配置 state 绑定"
    @click="emit('configure')"
  >
    <span>{{ label }}</span>
    <span v-if="nextExpiry" class="whitespace-nowrap font-mono text-cp-xs text-cp-text-secondary">{{ formatDateTime(nextExpiry) }} 到期</span>
  </button>
</template>
