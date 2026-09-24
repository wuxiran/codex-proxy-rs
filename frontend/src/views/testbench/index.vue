<script setup lang="ts">
// 测智台：对选中的一批账号并行发同一条测试 prompt（可选思考强度），并排看输出、人工判满血/降智。
// 状态与收流循环都在 stores/modules/testbench.ts：切走页面再回来，结果与进行中的生成都保留。
// 输出（常是 HTML/SVG）用 sandbox="" iframe 隔离预览：无脚本/无同源/无表单/无导航 + CSP 断远程资源。
import type { RunState, RunStatus, Verdict } from '@/stores/modules/testbench'
import { storeToRefs } from 'pinia'
import { onMounted } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseNumberInput from '@/components/base/BaseNumberInput.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { useTestBenchStore } from '@/stores/modules/testbench'

const store = useTestBenchStore()
const {
  groupId,
  model,
  effort,
  prompt,
  concurrency,
  groups,
  accountsLoading,
  search,
  runs,
  running,
  selected,
  selectedSet,
  filteredAccounts,
  allVisibleSelected,
} = storeToRefs(store)

const effortOptions = [
  { label: '思考强度：低', value: 'low' },
  { label: '思考强度：中', value: 'medium' },
  { label: '思考强度：高', value: 'high' },
  { label: '思考强度：xhigh', value: 'xhigh' },
  { label: '思考强度：max', value: 'max' },
]
const modelOptions = [
  { label: 'gpt-6-astra', value: 'gpt-6-astra' },
  { label: 'gpt-6-sol', value: 'gpt-6-sol' },
  { label: 'gpt-5.6-sol', value: 'gpt-5.6-sol' },
  { label: 'gpt-5.5', value: 'gpt-5.5' },
]

const CSP = '<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; style-src \'unsafe-inline\'; img-src data:; font-src data:;">'
function previewSrcdoc(out: string) {
  return out ? `${CSP}\n${out}` : ''
}

function groupOptions() {
  return [
    { label: '全部分组', value: '' },
    ...groups.value.map(g => ({ label: g.name, value: g.id })),
  ]
}

function downloadOutput(run: RunState) {
  const blob = new Blob([run.output], { type: 'text/html;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `testbench-${run.name || run.accountId}.html`
  a.click()
  URL.revokeObjectURL(url)
}

const statusMeta: Record<RunStatus, { text: string, cls: string }> = {
  queued: { text: '排队', cls: 'border-neutral-400/40 text-neutral-500' },
  running: { text: '生成中…', cls: 'border-amber-500/40 text-amber-500' },
  success: { text: '完成', cls: 'border-emerald-500/40 text-emerald-500' },
  empty: { text: '空响应', cls: 'border-neutral-400/40 text-neutral-500' },
  error: { text: '失败', cls: 'border-rose-500/40 text-rose-500' },
}

function verdictCls(run: RunState, v: Verdict) {
  if (run.verdict !== v)
    return 'text-neutral-500 ring-neutral-300 dark:ring-neutral-700'
  return v === 'full'
    ? 'bg-emerald-500/15 text-emerald-500 ring-emerald-500/40'
    : 'bg-rose-500/15 text-rose-500 ring-rose-500/40'
}

function runTime(run: RunState) {
  try {
    return new Date(run.startedAt).toLocaleTimeString()
  }
  catch {
    return ''
  }
}

onMounted(() => {
  store.reconcileInterrupted()
  void store.ensureLoaded()
})
</script>

<template>
  <div class="flex w-full flex-col gap-5 px-4 py-6">
    <BasePageHeader
      title="测智台"
      description="选一批账号并行发同一条测试 prompt（可选思考强度），并排看输出、人工判满血/降智。走 probe 路径钉住账号，钉票随账号自动带。切换页面不会丢结果。" />

    <BaseCard>
      <div class="flex flex-col gap-4">
        <!-- 参数行 -->
        <div class="flex flex-wrap items-end gap-3">
          <div class="w-48">
            <label class="block text-xs text-neutral-500">分组</label>
            <BaseSelect v-model="groupId" :options="groupOptions()" class="mt-1" @update:model-value="store.loadAccounts()" />
          </div>
          <div class="w-48">
            <label class="block text-xs text-neutral-500">模型</label>
            <BaseSelect v-model="model" :options="modelOptions" class="mt-1" />
          </div>
          <div class="w-40">
            <label class="block text-xs text-neutral-500">思考强度</label>
            <BaseSelect v-model="effort" :options="effortOptions" class="mt-1" />
          </div>
          <div class="w-32">
            <label class="block text-xs text-neutral-500">并发上限</label>
            <BaseNumberInput v-model="concurrency" label="并发上限" :min="1" :max="8" class="mt-1" />
          </div>
          <BaseButton :loading="running" :disabled="running || selected.length === 0" @click="store.runBatch()">
            开始测试（{{ selected.length }}）
          </BaseButton>
          <BaseButton v-if="runs.length" size="sm" variant="secondary" :disabled="running" @click="store.clearRuns()">
            清空结果
          </BaseButton>
        </div>

        <!-- prompt -->
        <div>
          <label class="block text-xs text-neutral-500">Prompt（所有选中账号共用）</label>
          <BaseTextarea v-model="prompt" :rows="3" class="mt-1" placeholder="输入测试 prompt" />
        </div>

        <!-- 账号选择 -->
        <div>
          <div class="mb-2 flex items-center gap-3">
            <label class="text-xs text-neutral-500">
              账号
              <span v-if="accountsLoading" class="ml-1 text-amber-500">加载中…</span>
              <span v-else class="ml-1 text-neutral-400">（{{ filteredAccounts.length }} 个，已选 {{ selected.length }}）</span>
            </label>
            <BaseInput v-model="search" placeholder="搜索名称/邮箱/ID" class="h-7 w-56 text-xs" />
            <BaseCheckbox
              :model-value="allVisibleSelected" label="全选可见" show-label
              class="text-xs" @update:model-value="store.toggleAllVisible" />
            <button class="text-xs text-neutral-500 hover:text-neutral-800 dark:hover:text-neutral-200" @click="store.clearSelection()">
              清空
            </button>
            <button class="text-xs text-neutral-500 hover:text-neutral-800 dark:hover:text-neutral-200" @click="store.loadAccounts()">
              刷新列表
            </button>
          </div>
          <div class="max-h-64 overflow-auto rounded-md border border-neutral-200 dark:border-neutral-800">
            <div
              v-for="a in filteredAccounts" :key="a.id"
              class="flex items-center gap-2 border-b border-neutral-100 px-3 py-1.5 text-sm last:border-b-0 dark:border-neutral-800/60">
              <BaseCheckbox
                :model-value="selectedSet.has(a.id)" :label="a.name || a.id"
                @update:model-value="(v: boolean) => store.toggle(a.id, v)" />
              <span class="min-w-0 flex-1 truncate">{{ a.name || a.id }}</span>
              <span class="shrink-0 text-xs text-neutral-400">{{ a.provider }}</span>
              <span
                class="shrink-0 rounded px-1.5 py-0.5 text-[10px]"
                :class="a.enabled ? 'text-emerald-500' : 'text-neutral-400'">
                {{ a.enabled ? '启用' : '停用' }}
              </span>
              <span
                v-for="g in a.groups" :key="g.id"
                class="shrink-0 rounded px-1.5 py-0.5 text-[10px] text-neutral-500 ring-1 ring-neutral-300 dark:ring-neutral-700">
                {{ g.name }}
              </span>
            </div>
            <div v-if="!accountsLoading && filteredAccounts.length === 0" class="px-3 py-6 text-center text-sm text-neutral-400">
              无账号
            </div>
          </div>
        </div>
      </div>
    </BaseCard>

    <!-- 结果并排 -->
    <div v-if="runs.length" class="grid gap-4 md:grid-cols-2 2xl:grid-cols-3">
      <BaseCard v-for="run in runs" :key="run.accountId + run.startedAt" class="flex flex-col">
        <template #header>
          <div class="flex flex-col gap-2">
            <div class="flex items-center justify-between gap-2">
              <div class="min-w-0 truncate text-sm font-semibold" :title="run.name">
                {{ run.name }}
              </div>
              <span class="shrink-0 rounded-full border px-2 py-0.5 text-[11px]" :class="statusMeta[run.status].cls">
                {{ statusMeta[run.status].text }}<template v-if="run.ms && run.status !== 'queued' && run.status !== 'running'"> · {{ (run.ms / 1000).toFixed(1) }}s</template>
              </span>
            </div>
            <div class="text-[11px] text-neutral-400">
              {{ run.model }} · {{ run.effort }} · {{ runTime(run) }}
            </div>
            <!-- 人工判定（独立于运行状态） -->
            <div class="flex items-center gap-1.5">
              <button
                class="rounded px-2 py-0.5 text-[11px] ring-1 transition hover:ring-emerald-400"
                :class="verdictCls(run, 'full')"
                @click="store.setVerdict(run, 'full')">
                满血
              </button>
              <button
                class="rounded px-2 py-0.5 text-[11px] ring-1 transition hover:ring-rose-400"
                :class="verdictCls(run, 'degraded')"
                @click="store.setVerdict(run, 'degraded')">
                降智
              </button>
              <BaseInput v-model="run.note" placeholder="备注" class="h-6 flex-1 text-[11px]" />
              <BaseButton v-if="run.output" size="sm" variant="secondary" @click="downloadOutput(run)">
                下载
              </BaseButton>
            </div>
          </div>
        </template>
        <template #body>
          <div v-if="run.errorMsg" class="mb-2 rounded-md border border-rose-500/30 bg-rose-500/5 px-3 py-2 text-xs text-rose-500">
            {{ run.errorMsg }}
          </div>
          <div v-if="run.output" class="flex flex-col gap-2">
            <iframe
              :srcdoc="previewSrcdoc(run.output)"
              sandbox=""
              class="h-80 w-full rounded-md border border-neutral-200 bg-white dark:border-neutral-800" />
            <details>
              <summary class="cursor-pointer text-xs text-neutral-500">原文（{{ run.output.length }} 字符）</summary>
              <pre class="mt-1 max-h-64 overflow-auto rounded-md border border-neutral-200 bg-neutral-50 p-2 text-[11px] leading-relaxed dark:border-neutral-800 dark:bg-neutral-900">{{ run.output }}</pre>
            </details>
          </div>
          <div v-else-if="run.status === 'running'" class="py-6 text-center text-sm text-neutral-500">
            生成中…
          </div>
          <div v-else-if="run.status === 'queued'" class="py-6 text-center text-sm text-neutral-400">
            排队中…
          </div>
          <div v-else-if="run.status === 'empty'" class="py-6 text-center text-sm text-neutral-500">
            上游未返回任何文本（可能被拒绝或空响应）。
          </div>
        </template>
      </BaseCard>
    </div>
  </div>
</template>
