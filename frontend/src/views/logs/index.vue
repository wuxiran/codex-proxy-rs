<script setup lang="ts">
// 请求日志台（表格 + 筛选 + 翻页）：逐请求观测统一 cookie 库（注入/沿用 __cf_bm）、
// turn-state 票、上游实际模型、service_tier、__cf_bm 签发 TTL——全部只作中性诊断事实。
// 数据源 /api/admin/logs/recent 为最近 300 条内存记录，筛选与翻页均在客户端进行。
// 满血/降智不由本页任何被动信号判定（TTL 判降智已被实测否定）；只如实铺数据。
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'
import type { BaseTablePagination as Pagination } from '@/components/base/BaseTable/pagination'
import { computed, onMounted, ref, watch } from 'vue'
import request from '@/api/request'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'

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

const allRows = ref<LogRow[]>(sample)
const usingSample = ref(true)
const loaded = ref(false)
const loadError = ref(false)
const loading = ref(false)

// —— 筛选状态 ——
const q = ref('')
const modelFilter = ref('')
const actionFilter = ref('')
const ttlFilter = ref('all')
const moleOnly = ref(false)

const page = ref(1)
const pageSize = ref(20)

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
    // 成功即以真数据为准（空数组也如实展示为「无记录」，不再拿示例/旧数据冒充当前）。
    allRows.value = Array.isArray(data) ? data.map(toRow) : []
    usingSample.value = false
    loaded.value = true
    loadError.value = false
  }
  catch {
    loadError.value = true
    // 从没成功过就清掉示例，避免把示例当成当前结果。
    if (usingSample.value)
      allRows.value = []
  }
  finally {
    loading.value = false
  }
}

// 空状态区分：加载中 / 加载失败 / 已加载但无记录（日志开关关闭或还没测试）。
const emptyText = computed(() => {
  if (loadError.value)
    return '加载失败，点「刷新」重试'
  if (!loaded.value)
    return '加载中…'
  return '暂无测试记录（可能日志开关关闭，或还没有测试）'
})

onMounted(load)

const actionLabel = (a: LogRow['action']) => (a === 'reuse' ? '沿用' : a === 'inject' ? '注入' : '无 cookie')
function actionClass(a: LogRow['action']) {
  return a === 'reuse' ? 'text-emerald-500' : a === 'inject' ? 'text-amber-500' : 'text-neutral-400'
}

// TTL 只作中性诊断数值展示（老板实测：TTL 判降智不科学），不再红黄绿上「降智」色。
function ttlClass(ttl?: number) {
  return ttl == null ? 'text-neutral-300 dark:text-neutral-600' : 'text-neutral-600 dark:text-neutral-300'
}

const isMole = (r: LogRow) => Boolean(r.servedModel && r.servedModel !== r.model)

// —— 筛选下拉选项（来自当前数据） ——
function distinct(getter: (r: LogRow) => string | undefined) {
  return [...new Set(allRows.value.map(getter).filter((v): v is string => Boolean(v)))].sort()
}
const modelOptions = computed(() => [{ label: '全部模型', value: '' }, ...distinct(r => r.model).map(v => ({ label: v, value: v }))])
const actionOptions = [
  { label: '全部决策', value: '' },
  { label: '沿用', value: 'reuse' },
  { label: '注入', value: 'inject' },
  { label: '无 cookie', value: 'none' },
]
const ttlOptions = [
  { label: '全部 TTL', value: 'all' },
  { label: '短 <300s', value: 'short' },
  { label: '中 300–900s', value: 'mid' },
  { label: '长 ≥900s', value: 'long' },
  { label: '无 TTL', value: 'none' },
]

function matchTtl(ttl: number | undefined, f: string) {
  if (f === 'all')
    return true
  if (f === 'none')
    return ttl == null
  if (ttl == null)
    return false
  if (f === 'short')
    return ttl < 300
  if (f === 'mid')
    return ttl >= 300 && ttl < 900
  return ttl >= 900
}

const filtered = computed(() => {
  const kw = q.value.trim().toLowerCase()
  return allRows.value.filter((r) => {
    if (modelFilter.value && r.model !== modelFilter.value)
      return false
    if (actionFilter.value && r.action !== actionFilter.value)
      return false
    if (!matchTtl(r.cfbmTtl, ttlFilter.value))
      return false
    if (moleOnly.value && !isMole(r))
      return false
    if (kw) {
      const hay = [r.unified, r.egress, r.ticketIn, r.ticketOut, r.servedModel, r.model, r.tier]
        .filter(Boolean)
        .join(' ')
        .toLowerCase()
      if (!hay.includes(kw))
        return false
    }
    return true
  })
})

const pagedRows = computed(() => {
  const start = (page.value - 1) * pageSize.value
  return filtered.value.slice(start, start + pageSize.value)
})

const pagination = computed<Pagination>(() => ({
  currentPage: page.value,
  pageSize: pageSize.value,
  total: filtered.value.length,
  pageSizes: [10, 20, 50, 100],
}))

function handlePageChange(p: number) {
  page.value = p
}
function handlePageSizeChange(s: number) {
  pageSize.value = s
  page.value = 1
}

// 筛选变化时回到第一页。
watch([q, modelFilter, actionFilter, ttlFilter, moleOnly], () => {
  page.value = 1
})

const stats = computed(() => {
  const withTtl = filtered.value.filter(r => r.cfbmTtl != null)
  return {
    total: filtered.value.length,
    reuse: filtered.value.filter(r => r.action === 'reuse').length,
    inject: filtered.value.filter(r => r.action === 'inject').length,
    shortTtl: withTtl.filter(r => (r.cfbmTtl as number) < 300).length,
    ttlSamples: withTtl.length,
  }
})
</script>

<template>
  <div class="mx-auto flex max-w-[1600px] flex-col gap-5 px-4 py-6">
    <BasePageHeader
      title="请求日志台"
      description="逐请求观测统一 cookie 库（注入/沿用 __cf_bm）、turn-state 票、上游实际模型、service_tier、__cf_bm 签发 TTL——均为中性诊断事实，不作满血/降智判定。"
    >
      <template #actions>
        <span
          class="rounded-full border px-2.5 py-1 font-mono text-[11px]"
          :class="usingSample ? 'border-amber-500/40 text-amber-500' : 'border-emerald-500/40 text-emerald-500'"
        >
          {{ usingSample ? '示例数据 · 暂无实时' : '实时' }}
        </span>
        <button
          class="rounded-full border border-neutral-300 px-2.5 py-1 text-[11px] text-neutral-600 hover:border-neutral-400 disabled:opacity-50 dark:border-neutral-700 dark:text-neutral-300"
          :disabled="loading" @click="load"
        >
          {{ loading ? '刷新中…' : '↻ 刷新' }}
        </button>
      </template>
    </BasePageHeader>

    <div class="grid grid-cols-2 gap-3 sm:grid-cols-4">
      <BaseCard>
        <div class="text-xs text-neutral-500">
          筛选后请求
        </div>
        <div class="mt-2 text-2xl font-semibold tabular-nums">
          {{ stats.total }}
        </div>
      </BaseCard>
      <BaseCard>
        <div class="text-xs text-neutral-500">
          统一库沿用
        </div>
        <div class="mt-2 text-2xl font-semibold tabular-nums text-emerald-500">
          {{ stats.reuse }}
        </div>
      </BaseCard>
      <BaseCard>
        <div class="text-xs text-neutral-500">
          统一库注入
        </div>
        <div class="mt-2 text-2xl font-semibold tabular-nums text-amber-500">
          {{ stats.inject }}
        </div>
      </BaseCard>
      <BaseCard>
        <div class="text-xs text-neutral-500">
          短 TTL(&lt;300s) 计数
        </div>
        <div class="mt-2 text-2xl font-semibold tabular-nums text-neutral-600 dark:text-neutral-300">
          {{ stats.shortTtl }}<span class="ml-1 text-sm font-normal text-neutral-400">/ {{ stats.ttlSamples }} 有 TTL</span>
        </div>
      </BaseCard>
    </div>

    <BaseCard padding="none">
      <div class="flex flex-wrap items-center gap-2 border-b border-neutral-200 px-4 py-3 dark:border-neutral-800">
        <BaseInput v-model="q" placeholder="搜索 cf指纹 / 出口 / 票 / 实际模型…" class="w-60" />
        <BaseSelect v-model="modelFilter" :options="modelOptions" class="w-40" />
        <BaseSelect v-model="actionFilter" :options="actionOptions" class="w-32" />
        <BaseSegmented v-model="ttlFilter" label="TTL 档" :options="ttlOptions" />
        <BaseCheckbox v-model="moleOnly" label="只看实际模型≠请求" class="ml-1 text-xs" />
      </div>
      <BaseTable :columns="columns" :rows="pagedRows" row-key="id" :loading="loading" density="compact" :empty-text="allRows.length ? '无匹配请求（可调整筛选）' : emptyText">
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
          <span
            v-if="(row as LogRow).servedModel"
            class="font-mono text-xs"
            :class="isMole(row as LogRow) ? 'font-semibold text-amber-600 dark:text-amber-400' : 'text-neutral-500'"
          >
            {{ (row as LogRow).servedModel }}<span v-if="isMole(row as LogRow)" class="text-neutral-400"> ≠请求</span>
          </span>
          <span v-else class="text-neutral-300 dark:text-neutral-600">—</span>
        </template>
        <template #tier="{ row }">
          <span
            v-if="(row as LogRow).tier"
            class="whitespace-nowrap rounded-md border px-1.5 py-0.5 font-mono text-xs"
            :class="(row as LogRow).tier === 'priority' ? 'border-emerald-500/35 text-emerald-600 dark:text-emerald-400' : 'border-neutral-300 text-neutral-500 dark:border-neutral-700'"
          >
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
      <BaseTablePagination
        :pagination="pagination"
        :loading="loading"
        @page-change="handlePageChange"
        @page-size-change="handlePageSizeChange"
      />
    </BaseCard>
  </div>
</template>
