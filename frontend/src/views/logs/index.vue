<script setup lang="ts">
// 请求日志台（表格）：逐请求观测统一 cookie 库（注入/沿用 __cf_bm）、turn-state 票、
// 上游实际模型(分叉=猫腻)、service_tier(诊断)、以及 __cf_bm 的签发 TTL（短=降智节点特征）。
// 只如实铺数据；TTL 仅按阈值上色，不替用户下「降智」结论。
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'
import { computed, onMounted, ref } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
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
  servedModel?: string
  respCookies?: string[]
  cfbmTtl?: number
}

interface LogRow {
  id: number
  time: string
  action: 'reuse' | 'inject' | 'none'
  unified?: string
  egress: string
  model: string
  servedModel?: string
  tier?: string
  ticketIn?: string
  ticketOut?: string
  ticketLen?: number
  cfbmTtl?: number
  setCookie?: boolean
  respCookies?: string[]
}

const columns: BaseTableColumn<LogRow>[] = [
  { key: 'time', label: '时间', size: 'sm' },
  { key: 'action', label: '决策', kind: 'custom', size: 'sm' },
  { key: 'unified', label: 'cf 指纹', kind: 'custom', size: 'sm' },
  { key: 'egress', label: '出口', kind: 'custom', size: 'sm' },
  { key: 'model', label: '请求模型', kind: 'custom' },
  { key: 'servedModel', label: '实际模型', kind: 'custom' },
  { key: 'tier', label: '档位', kind: 'custom', size: 'sm' },
  { key: 'ticket', label: '票 (in → out)', kind: 'custom', size: 'xl' },
  { key: 'ticketLen', label: '票长', align: 'right', size: 'sm' },
  { key: 'cfbmTtl', label: '__cf_bm TTL', kind: 'custom', align: 'right', size: 'sm' },
  { key: 'setCookie', label: 'Set-Cookie', kind: 'custom', size: 'xl' },
]

const sample: LogRow[] = [
  { id: 1, time: '01:34:26', action: 'reuse', unified: 'cfbm-802', egress: 'egr-14', model: 'gpt-6-sol', servedModel: 'gpt-6-sol', tier: 'default', ticketIn: '#7387ec', ticketOut: '#a1f3c0', ticketLen: 780, cfbmTtl: 1798, setCookie: true, respCookies: ['__cf_bm@chatgpt.com'] },
  { id: 2, time: '01:34:17', action: 'inject', unified: 'cfbm-73', egress: 'egr-9', model: 'gpt-6-sol', servedModel: 'gpt-6-luna', tier: 'default', ticketIn: '#6a82b2', ticketOut: '#6a82b2', ticketLen: 780, cfbmTtl: 120, setCookie: true, respCookies: ['__cf_bm@chatgpt.com'] },
  { id: 3, time: '01:33:58', action: 'none', egress: 'egr-3', model: 'gpt-6-sol' },
]

const rows = ref<LogRow[]>(sample)
const usingSample = ref(true)
const loading = ref(false)

function fmtTime(ms: number): string {
  try {
    return new Date(ms).toLocaleTimeString('zh-CN', { hour12: false })
  }
  catch {
    return '--:--:--'
  }
}

function toRow(r: BackendRecord, i: number): LogRow {
  return {
    id: i,
    time: fmtTime(r.atMs),
    action: r.cookieAction,
    unified: r.unified,
    egress: r.egress,
    model: r.model,
    servedModel: r.servedModel,
    tier: r.serviceTier,
    ticketIn: r.ticketIn,
    ticketOut: r.ticketOut,
    ticketLen: r.ticketLen,
    cfbmTtl: r.cfbmTtl,
    setCookie: r.setCookie,
    respCookies: r.respCookies,
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
  }
  catch {
    // 后端未接入或无数据：保留示例并明确标注。
  }
  finally {
    loading.value = false
  }
}

onMounted(load)

const actionLabel = (a: LogRow['action']) => (a === 'reuse' ? '沿用' : a === 'inject' ? '注入' : '无 cookie')
const actionClass = (a: LogRow['action']) =>
  a === 'reuse' ? 'text-emerald-500' : a === 'inject' ? 'text-amber-500' : 'text-neutral-400'

// TTL 上色：短(<300s)=红(降智节点特征)，中(<900s)=黄，长(~1800s)=绿。仅上色，不下结论。
function ttlClass(ttl?: number) {
  if (ttl == null)
    return 'text-neutral-300 dark:text-neutral-600'
  if (ttl < 300)
    return 'text-rose-500 font-semibold'
  if (ttl < 900)
    return 'text-amber-500'
  return 'text-emerald-500'
}

// 实际模型 != 请求模型 = 猫腻（掺假/relay/降级上报）。
const isMole = (r: LogRow) => Boolean(r.servedModel && r.servedModel !== r.model)

const stats = computed(() => {
  const withTtl = rows.value.filter(r => r.cfbmTtl != null)
  return {
    total: rows.value.length,
    reuse: rows.value.filter(r => r.action === 'reuse').length,
    inject: rows.value.filter(r => r.action === 'inject').length,
    shortTtl: withTtl.filter(r => (r.cfbmTtl as number) < 300).length,
    ttlSamples: withTtl.length,
  }
})
</script>

<template>
  <div class="mx-auto flex max-w-[1600px] flex-col gap-5 px-4 py-6">
    <BasePageHeader
      title="请求日志台"
      description="逐请求观测统一 cookie 库（注入/沿用 __cf_bm）、turn-state 票、上游实际模型（分叉=猫腻）、service_tier（诊断）与 __cf_bm 签发 TTL（短=降智节点特征）。">
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
        <div class="text-xs text-neutral-500">短 TTL（降智嫌疑）</div>
        <div class="mt-2 text-2xl font-semibold tabular-nums" :class="stats.shortTtl ? 'text-rose-500' : 'text-neutral-400'">
          {{ stats.shortTtl }}<span class="ml-1 text-sm font-normal text-neutral-400">/ {{ stats.ttlSamples }} 有 TTL</span>
        </div>
      </BaseCard>
    </div>

    <BaseCard :padding="'none'">
      <div class="border-b border-neutral-200 px-4 py-3 text-xs text-neutral-500 dark:border-neutral-800">
        最近请求 · 票为短哈希非原文 · 请求侧发出即记，响应侧（Set-Cookie/票长/档位/实际模型/TTL）于上游响应回来后回填
      </div>
      <BaseTable :columns="columns" :rows="rows" row-key="id" :loading="loading" density="compact" empty-text="暂无请求">
        <template #action="{ row }">
          <span class="font-semibold" :class="actionClass((row as LogRow).action)">{{ actionLabel((row as LogRow).action) }}</span>
        </template>
        <template #unified="{ row }">
          <span v-if="(row as LogRow).unified" class="font-mono text-xs text-blue-500">{{ (row as LogRow).unified }}</span>
          <span v-else class="text-neutral-300 dark:text-neutral-600">—</span>
        </template>
        <template #egress="{ row }">
          <span class="font-mono text-xs text-neutral-500">{{ (row as LogRow).egress }}</span>
        </template>
        <template #model="{ row }">
          <span class="font-mono text-xs font-semibold">{{ (row as LogRow).model }}</span>
        </template>
        <template #servedModel="{ row }">
          <span v-if="(row as LogRow).servedModel"
            class="font-mono text-xs"
            :class="isMole(row as LogRow) ? 'font-semibold text-rose-500' : 'text-neutral-500'">
            {{ (row as LogRow).servedModel }}<span v-if="isMole(row as LogRow)"> ⚠猫腻</span>
          </span>
          <span v-else class="text-neutral-300 dark:text-neutral-600">—</span>
        </template>
        <template #tier="{ row }">
          <span v-if="(row as LogRow).tier"
            class="whitespace-nowrap rounded-md border px-1.5 py-0.5 font-mono text-xs"
            :class="(row as LogRow).tier === 'priority' ? 'border-emerald-500/35 text-emerald-600 dark:text-emerald-400' : 'border-neutral-300 text-neutral-500 dark:border-neutral-700'">
            {{ (row as LogRow).tier }}
          </span>
          <span v-else class="text-neutral-300 dark:text-neutral-600">—</span>
        </template>
        <template #ticket="{ row }">
          <span v-if="(row as LogRow).ticketIn || (row as LogRow).ticketOut" class="whitespace-nowrap font-mono text-xs text-amber-600 dark:text-amber-400">
            {{ (row as LogRow).ticketIn || '—' }}<template v-if="(row as LogRow).ticketOut"> → {{ (row as LogRow).ticketOut }}</template>
          </span>
          <span v-else class="text-neutral-300 dark:text-neutral-600">未带票</span>
        </template>
        <template #cfbmTtl="{ row }">
          <span class="font-mono text-xs tabular-nums" :class="ttlClass((row as LogRow).cfbmTtl)">
            {{ (row as LogRow).cfbmTtl != null ? `${(row as LogRow).cfbmTtl}s` : '—' }}
          </span>
        </template>
        <template #setCookie="{ row }">
          <template v-if="(row as LogRow).setCookie === undefined">
            <span class="text-neutral-300 dark:text-neutral-600">响应侧待回填</span>
          </template>
          <template v-else>
            <span :class="(row as LogRow).setCookie ? 'text-emerald-500' : 'text-neutral-400'" class="text-xs">
              {{ (row as LogRow).setCookie ? '含 __cf_bm' : '无' }}
            </span>
            <span v-if="(row as LogRow).respCookies?.length" class="ml-1 font-mono text-[11px] text-neutral-400" :title="(row as LogRow).respCookies?.join('\n')">
              {{ (row as LogRow).respCookies?.map(c => c.split('#')[0]).join(', ') }}
            </span>
          </template>
        </template>
      </BaseTable>
    </BaseCard>
  </div>
</template>
