<script setup lang="ts">
// fork: proxy-quality — 列表里的质量徽标：检测中 / 等级评分（点击看报告）；无快照时不占位。
import type { OutboundProxyRecord } from '@/api'
import { qualityStatus } from '../presenter.fork'

defineProps<{
  proxy: OutboundProxyRecord
  checking: boolean
}>()

const emit = defineEmits<{
  open: [proxy: OutboundProxyRecord]
}>()
</script>

<template>
  <span v-if="checking" class="text-cp-xs text-cp-text-secondary">质量检测中</span>
  <button
    v-else-if="proxy.quality"
    type="button"
    class="inline-flex cursor-pointer items-center gap-1 rounded-cp border-0 px-1.5 py-0.5 text-cp-xs font-bold outline-none transition-opacity hover:opacity-80 focus-visible:ring-2 focus-visible:ring-cp-control-outline"
    :class="qualityStatus(proxy.quality.status).badge"
    :title="`${proxy.quality.summary}，点击查看报告`"
    @click="emit('open', proxy)"
  >
    <span>{{ qualityStatus(proxy.quality.status).label }}</span>
    <span class="font-mono tabular-nums">{{ proxy.quality.grade }}/{{ proxy.quality.score }}</span>
  </button>
</template>
