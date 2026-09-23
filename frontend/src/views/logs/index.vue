<script setup lang="ts">
// 请求日志台：统一 cookie 库（注入/沿用 __cf_bm）+ turn-state 票据 逐请求观测。
// 请求侧（selector）：cookie 决策 / 统一库短 id / 入票指纹 / 模型 / 出口。
// 响应侧（execution，上游响应回来后回填）：Set-Cookie 是否含 __cf_bm / 上游票短指纹与票长 /
// 真实 service_tier（default/flex/priority）——满血与否只认这个字段，绝不从模型名臆断。
import { computed, onMounted, ref } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import request from '@/api/request'

interface BackendRecord {
  atMs: number
  model: string
  cookieAction: 'reuse' | 'inject' | 'none'
  egress: string
  unified?: string
  ticketIn?: string
  setCookie?: boolean
  ticketOut?: string
  ticketLen?: number
  serviceTier?: string
}

interface LogRow {
  at: string
  action: 'reuse' | 'inject' | 'none'
  unified?: string
  ticket?: string
  model: string
  egress: string
  setCookie?: boolean
  ticketOut?: string
  ticketLen?: number
  tier?: string
}

const sample: LogRow[] = [
  { at: '01:34:26', action: 'reuse', unified: 'unified-802', ticket: '#7387ec', model: 'gpt-6-sol', egress: 'egr-14', setCookie: true, ticketOut: '#a1f3c0', ticketLen: 780, tier: 'priority' },
  { at: '01:34:23', action: 'reuse', unified: 'unified-802', ticket: '#7387ec', model: 'gpt-5.6-sol', egress: 'egr-14', setCookie: false, ticketOut: '#a1f3c0', ticketLen: 780, tier: 'default' },
  { at: '01:34:17', action: 'inject', unified: 'unified-73', ticket: '#6a82b2', model: 'gpt-6-sol', egress: 'egr-9', setCookie: true, ticketOut: '#6a82b2', ticketLen: 780, tier: 'priority' },
  { at: '01:33:58', action: 'none', model: 'gpt-6-sol', egress: 'egr-3' },
]

const rows = ref<LogRow[]>(sample)
const usingSample = ref(true)
const loading = ref(false)

function fmtTime(ms: number): string {
  try {
    return new Date(ms).toLocaleTimeString('zh-CN', { hour12: false })
  } catch {
    return '--:--:--'
  }
}

function toRow(r: BackendRecord): LogRow {
  return {
    at: fmtTime(r.atMs),
    action: r.cookieAction,
    unified: r.unified,
    ticket: r.ticketIn,
    model: r.model,
    egress: r.egress,
    setCookie: r.setCookie,
    ticketOut: r.ticketOut,
    ticketLen: r.ticketLen,
    tier: r.serviceTier,
  }
}

async function load() {
  loading.value = true
  try {
    const data = await request<BackendRecord[]>({ url: '/api/admin/logs/recent', method: 'GET' })
    if (Array.isArray(data) && data.length) {
      rows.value = data.map(toRow)
      usingSample.value = false
    }
  } catch {
    // 后端未接入或无数据：保留示例并明确标注。
  } finally {
    loading.value = false
  }
}

onMounted(load)

const actionLabel = (a: LogRow['action']) => (a === 'reuse' ? '沿用' : a === 'inject' ? '注入' : '无 cookie')
const actionClass = (a: LogRow['action']) =>
  a === 'reuse' ? 'text-emerald-500' : a === 'inject' ? 'text-amber-500' : 'text-neutral-400'

// service_tier 是满血与否的唯一可信信号：priority=满血档，default/flex=普通档。
// 只统计「已回填 tier 的响应」，未回填（请求侧或失败无响应）的不计入分母，避免虚报。
const stats = computed(() => {
  const withTier = rows.value.filter(r => r.tier)
  return {
    total: rows.value.length,
    reuse: rows.value.filter(r => r.action === 'reuse').length,
    inject: rows.value.filter(r => r.action === 'inject').length,
    priority: withTier.filter(r => r.tier === 'priority').length,
    tiered: withTier.length,
  }
})
</script>

<template>
  <div class="mx-auto flex max-w-6xl flex-col gap-5 px-4 py-6">
    <BasePageHeader
      title="请求日志台"
      description="逐请求观测统一 cookie 库（注入/沿用 __cf_bm）、turn-state 票指纹，与上游真实 service_tier（满血档只认此字段，不从模型名臆断）。">
      <template #actions>
        <span
          class="rounded-full border px-2.5 py-1 font-mono text-[11px]"
          :class="usingSample ? 'border-amber-500/40 text-amber-500' : 'border-emerald-500/40 text-emerald-500'">
          {{ usingSample ? '示例数据 · 暂无实时' : '实时' }}
        </span>
        <button
          class="rounded-full border border-neutral-300 px-2.5 py-1 text-[11px] text-neutral-600 hover:border-neutral-400 disabled:opacity-50 dark:border-neutral-700 dark:text-neutral-300"
          :disabled="loading" @click="load">
          {{ loading ? '刷新中…' : '↻ 刷新' }}
        </button>
      </template>
    </BasePageHeader>

    <div class="grid grid-cols-2 gap-3 sm:grid-cols-4">
      <BaseCard>
        <div class="text-xs text-neutral-500">本页请求</div>
        <div class="mt-2 text-2xl font-semibold tabular-nums">{{ stats.total }}</div>
      </BaseCard>
      <BaseCard>
        <div class="text-xs text-neutral-500">统一库沿用</div>
        <div class="mt-2 text-2xl font-semibold tabular-nums text-emerald-500">{{ stats.reuse }}</div>
      </BaseCard>
      <BaseCard>
        <div class="text-xs text-neutral-500">统一库注入</div>
        <div class="mt-2 text-2xl font-semibold tabular-nums text-amber-500">{{ stats.inject }}</div>
      </BaseCard>
      <BaseCard>
        <div class="text-xs text-neutral-500">priority 满血档</div>
        <div class="mt-2 text-2xl font-semibold tabular-nums text-emerald-500">
          {{ stats.priority }}<span class="ml-1 text-sm font-normal text-neutral-400">/ {{ stats.tiered }} 有档</span>
        </div>
      </BaseCard>
    </div>

    <BaseCard :padding="'none'">
      <div class="border-b border-neutral-200 px-4 py-3 text-xs text-neutral-500 dark:border-neutral-800">
        最近请求 · 票为短哈希非原文 · 请求侧发出即记，响应侧（Set-Cookie/票长/档位）于上游响应回来后回填
      </div>
      <div v-for="(r, i) in rows" :key="i"
        class="grid grid-cols-[64px_1fr] gap-3 border-b border-neutral-100 px-4 py-3 last:border-b-0 dark:border-neutral-800"
        :class="{ 'opacity-60': r.action === 'none' }">
        <div class="font-mono text-xs leading-6 text-neutral-400">{{ r.at }}</div>
        <div>
          <div class="flex flex-wrap items-center gap-2 text-sm">
            <span class="font-semibold" :class="actionClass(r.action)">{{ actionLabel(r.action) }}</span>
            <template v-if="r.action !== 'none'">
              <span class="text-neutral-400">cookie</span>
              <span v-if="r.unified" class="font-mono text-blue-500">{{ r.unified }}</span>
            </template>
            <template v-if="r.ticket || r.ticketOut">
              <span class="text-neutral-400">票</span>
              <span class="whitespace-nowrap rounded-md border border-amber-500/35 bg-amber-500/10 px-1.5 py-0.5 font-mono text-xs text-amber-600 dark:text-amber-400">
                {{ r.ticket || '—' }}<template v-if="r.ticketOut"> → {{ r.ticketOut }}</template>
              </span>
            </template>
            <span class="text-neutral-400">模型</span>
            <span class="font-mono font-semibold">{{ r.model }}</span>
            <template v-if="r.tier">
              <span class="whitespace-nowrap rounded-md border px-1.5 py-0.5 font-mono text-xs"
                :class="r.tier === 'priority' ? 'border-emerald-500/35 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400' : 'border-neutral-300 text-neutral-500 dark:border-neutral-700'">
                {{ r.tier === 'priority' ? '满血·priority' : r.tier }}
              </span>
            </template>
          </div>
          <div class="mt-1.5 flex flex-wrap items-center gap-1.5 font-mono text-xs text-neutral-500">
            <span>出口 {{ r.egress }}</span>
            <template v-if="r.action === 'none'">
              <span class="text-neutral-300 dark:text-neutral-600">·</span>
              <span>该出口池无新鲜 __cf_bm</span>
            </template>
            <template v-if="r.setCookie !== undefined">
              <span class="text-neutral-300 dark:text-neutral-600">·</span>
              <span :class="r.setCookie ? 'text-emerald-500' : 'text-neutral-400'">Set-Cookie {{ r.setCookie ? '含 __cf_bm' : '无' }}</span>
            </template>
            <template v-if="r.ticketLen">
              <span class="text-neutral-300 dark:text-neutral-600">·</span>
              <span>票长 {{ r.ticketLen }}</span>
            </template>
            <template v-if="r.action !== 'none' && r.setCookie === undefined">
              <span class="text-neutral-300 dark:text-neutral-600">·</span>
              <span class="text-neutral-400">响应侧待回填</span>
            </template>
          </div>
        </div>
      </div>
    </BaseCard>
  </div>
</template>
