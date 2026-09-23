<script setup lang="ts">
// 请求日志台：统一 cookie 库 / turn-state 票 / 满血(astra)·降智(luna) 逐请求观测。
// v1 先渲染结构与示例数据；实时数据待后端 GET /api/admin/logs/recent 接入。
import { computed, ref } from 'vue'

interface LogRow {
  at: string
  action: 'reuse' | 'inject' | 'off'
  unifiedFrom?: string
  unifiedTo?: string
  ticketFrom?: string
  ticketTo?: string
  ticketLen?: number
  model: string
  degraded?: boolean
  setCookieCf?: boolean
  buffer?: string
  premium?: boolean
  ticketAge?: string
  gatewayReturned?: boolean
  note?: string
}

// TODO: 换成 fetch('/api/admin/logs/recent')；后端环形缓冲接入前先示例占位。
const sample: LogRow[] = [
  { at: '05:18:16', action: 'reuse', unifiedTo: 'unified-88', ticketFrom: '#hPdTPIqq', ticketTo: '#Y89E_-KZ', ticketLen: 780, model: 'gpt-6-sol', setCookieCf: true, premium: true, ticketAge: '22s', gatewayReturned: true },
  { at: '05:16:59', action: 'reuse', unifiedTo: 'unified-88', ticketFrom: '#hPdTPIqq', ticketTo: '#arbCpbeU', ticketLen: 780, model: 'gpt-6-sol', setCookieCf: true, premium: true, gatewayReturned: true },
  { at: '05:14:52', action: 'reuse', unifiedTo: 'unified-94', ticketFrom: '#Z0VHPLVA', ticketTo: '#PPEqnG72', ticketLen: 780, model: 'gpt-6-sol', setCookieCf: true, premium: true, buffer: 'gpt-6-luna', gatewayReturned: true },
  { at: '05:12:44', action: 'inject', unifiedFrom: 'unified-199', unifiedTo: 'unified-88', ticketFrom: '#hPdTPIqq', ticketTo: '#hkG-Wjhr', ticketLen: 780, model: 'gpt-6-sol', setCookieCf: true, note: '账号无新鲜 cf_bm，从池借一张' },
  { at: '05:10:46', action: 'reuse', unifiedTo: 'unified-88', ticketFrom: '#TjTpIlK6', ticketTo: '#dYhP3PGc', ticketLen: 780, model: 'gpt-5.6-luna', degraded: true, setCookieCf: true, note: '对话过长 → 降智' },
  { at: '04:59:00', action: 'off', model: 'gpt-6-luna', degraded: true, note: '注入已关 · 未走统一库' },
  { at: '04:58:12', action: 'off', model: '未返回', gatewayReturned: false, note: '这发吃进去了，仍可能正常' },
]

const rows = ref<LogRow[]>(sample)
const usingSample = ref(true)

const actionLabel = (a: LogRow['action']) => (a === 'reuse' ? '沿用' : a === 'inject' ? '注入' : '注入已关')
const actionClass = (a: LogRow['action']) =>
  a === 'reuse' ? 'text-emerald-500' : a === 'inject' ? 'text-amber-500' : 'text-neutral-400'

const stats = computed(() => {
  const total = rows.value.length
  const reuse = rows.value.filter(r => r.action === 'reuse').length
  const full = rows.value.filter(r => !r.degraded && r.model.startsWith('gpt')).length
  return { total, reuse, full }
})
</script>

<template>
  <div class="mx-auto max-w-6xl px-4 py-6">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div>
        <h1 class="text-xl font-bold tracking-tight">请求日志台</h1>
        <p class="mt-1 max-w-2xl text-sm text-neutral-500 dark:text-neutral-400">
          逐请求观测统一 cookie 库（注入/沿用 <code class="font-mono">__cf_bm</code>）、turn-state 票指纹流转、满血(astra)/降智(luna) 与缓冲后备。
        </p>
      </div>
      <span v-if="usingSample"
        class="rounded-full border border-amber-500/40 px-2.5 py-1 font-mono text-[11px] text-amber-500">
        示例数据 · 后端接入中
      </span>
    </div>

    <!-- KPI -->
    <div class="mt-5 grid grid-cols-2 gap-3 sm:grid-cols-3">
      <div class="rounded-xl border border-neutral-200 bg-white p-4 dark:border-neutral-800 dark:bg-neutral-900">
        <div class="text-xs text-neutral-500">本页请求</div>
        <div class="mt-2 text-2xl font-semibold tabular-nums">{{ stats.total }}</div>
      </div>
      <div class="rounded-xl border border-neutral-200 bg-white p-4 dark:border-neutral-800 dark:bg-neutral-900">
        <div class="text-xs text-neutral-500">统一库沿用</div>
        <div class="mt-2 text-2xl font-semibold tabular-nums text-blue-500">{{ stats.reuse }}</div>
      </div>
      <div class="rounded-xl border border-neutral-200 bg-white p-4 dark:border-neutral-800 dark:bg-neutral-900">
        <div class="text-xs text-neutral-500">满血 (非 luna)</div>
        <div class="mt-2 text-2xl font-semibold tabular-nums text-emerald-500">{{ stats.full }}</div>
      </div>
    </div>

    <!-- 日志列表 -->
    <div class="mt-6 overflow-hidden rounded-xl border border-neutral-200 bg-white dark:border-neutral-800 dark:bg-neutral-900">
      <div class="flex flex-wrap items-center justify-between gap-2 border-b border-neutral-200 px-4 py-3 text-xs text-neutral-500 dark:border-neutral-800">
        <span>最近请求 · 票指纹为短哈希非原文</span>
        <span>不返票 = 这发吃进去了，仍可能正常 · 缓冲 faster-model 是降智后备</span>
      </div>

      <div v-for="(r, i) in rows" :key="i"
        class="grid grid-cols-[64px_1fr] gap-3 border-b border-neutral-100 px-4 py-3 last:border-b-0 dark:border-neutral-800"
        :class="{ 'opacity-60': r.action === 'off' }">
        <div class="font-mono text-xs leading-6 text-neutral-400">{{ r.at }}</div>
        <div>
          <div class="flex flex-wrap items-center gap-2 text-sm">
            <span class="font-semibold" :class="actionClass(r.action)">{{ actionLabel(r.action) }}</span>
            <span v-if="r.action !== 'off'" class="text-neutral-400">cookie</span>
            <span v-if="r.unifiedFrom" class="font-mono text-amber-500">{{ r.unifiedFrom }}</span>
            <span v-if="r.unifiedFrom" class="text-neutral-400">得到</span>
            <span v-if="r.unifiedTo" class="font-mono text-blue-500">{{ r.unifiedTo }}</span>
            <template v-if="r.ticketFrom">
              <span class="text-neutral-400">票</span>
              <span class="whitespace-nowrap rounded-md border border-amber-500/35 bg-amber-500/10 px-1.5 py-0.5 font-mono text-xs text-amber-600 dark:text-amber-400">
                {{ r.ticketFrom }} → {{ r.ticketTo }}
              </span>
            </template>
            <span class="text-neutral-400">模型</span>
            <span class="font-mono font-semibold" :class="r.degraded ? 'text-amber-500' : ''">{{ r.model }}</span>
          </div>
          <div class="mt-1.5 flex flex-wrap items-center gap-1.5 font-mono text-xs text-neutral-500">
            <span :class="r.setCookieCf ? 'text-emerald-500' : 'text-neutral-400'">
              Set-Cookie {{ r.setCookieCf ? '有 __cf_bm' : '无' }}
            </span>
            <span class="text-neutral-300 dark:text-neutral-600">·</span>
            <span>票长 {{ r.ticketLen ?? '—' }}</span>
            <template v-if="r.ticketAge"><span class="text-neutral-300 dark:text-neutral-600">·</span><span>票龄 {{ r.ticketAge }}</span></template>
            <template v-if="r.buffer"><span class="text-neutral-300 dark:text-neutral-600">·</span><span class="text-amber-500">缓冲 {{ r.buffer }}</span></template>
            <template v-if="r.premium"><span class="text-neutral-300 dark:text-neutral-600">·</span><span class="text-violet-500">premium</span></template>
            <template v-if="r.note"><span class="text-neutral-300 dark:text-neutral-600">·</span><span>{{ r.note }}</span></template>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
