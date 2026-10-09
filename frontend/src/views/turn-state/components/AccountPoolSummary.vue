<script setup lang="ts">
import type { PoolAccount } from '@/api'
import { computed } from 'vue'
import { formatDateTime } from '@/utils/format'
import { latestAttempt, poolPhaseText, remaining } from '../utils/poolOverview'

const props = defineProps<{ account?: PoolAccount, stale: boolean, paused: boolean, now: number, expanded?: boolean }>()
const last = computed(() => props.account ? latestAttempt(props.account) : undefined)
const cooldown = computed(() => Math.max(props.account?.mint.cooldownUntilMs ?? 0, props.account?.warm.cooldownUntilMs ?? 0))
const current = computed(() => [...new Set(props.account?.connections.filter(connection => connection.available && connection.expiresAtMs > props.now).map(connection => connection.gateway).filter(Boolean) ?? [])])
const phase = computed(() => props.stale ? '状态已过期' : props.paused ? '显示已暂停' : props.account ? poolPhaseText[props.account.phase] : '未取得运行状态')
</script>

<template>
  <div class="grid min-w-0 gap-1 text-cp-sm" data-testid="account-pool-summary">
    <span :class="!stale && !paused && account?.phase === 'ready' ? 'text-cp-success' : 'text-cp-text-secondary'" role="status">{{ phase }}</span>
    <template v-if="account">
      <span v-if="current.length" class="truncate font-mono text-cp-xs" :title="current.join(' · ')">{{ current.join(' · ') }}</span>
      <span v-if="!stale && cooldown > now" class="text-cp-xs text-cp-warning">冷却剩余 {{ remaining(cooldown, now) }}</span>
      <span v-if="last" class="text-cp-xs text-cp-text-secondary">
        最近{{ last.kind === 'mint' ? '打票' : '验证' }} {{ last.attempts }} 次尝试
      </span>
      <template v-if="expanded && last">
        <span class="text-cp-xs text-cp-text-secondary">{{ formatDateTime(new Date(last.atMs).toISOString()) }}</span>
        <span v-if="last.error" class="break-words text-cp-xs text-cp-error">{{ last.error }}</span>
        <span v-if="last.gateway && !current.includes(last.gateway)" class="text-cp-xs text-cp-text-secondary">最近尝试 {{ last.gateway }}</span>
      </template>
    </template>
  </div>
</template>
