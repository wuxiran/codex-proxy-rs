<script setup lang="ts">
// 测智台：对选中的一批账号并行发同一条测试 prompt（可选思考强度），并排看输出、人工判满血/降智。
// 走后台 probe 路径钉住账号，钉票随账号自动带。前端有界并发扇出（每账号一路 SSE），
// 每个 probe 在后端各自钉账号、受账号租约/并发约束；客户端再限一层并发上限。
// 输出（常是 HTML/SVG）用 sandbox="" iframe 隔离预览：无脚本/无同源/无表单/无导航 + CSP 断远程资源。
import type { Account, AccountGroup } from '@/api'
import { computed, onMounted, ref } from 'vue'
import { getAccountGroups, getAccounts } from '@/api'
import { API_BASE_URL } from '@/api/constants'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseNumberInput from '@/components/base/BaseNumberInput.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { toast } from '@/components/base/BaseToast'

const PELICAN_PROMPT
  = 'Create an HTML page with an SVG drawing of a pelican riding a bicycle in 2D. Output only the HTML, no explanation, no markdown fences.'

type RunStatus = 'queued' | 'running' | 'success' | 'empty' | 'error'
type Verdict = 'unknown' | 'full' | 'degraded'

interface RunState {
  accountId: string
  name: string
  status: RunStatus
  output: string
  errorMsg: string
  verdict: Verdict
  note: string
  ms: number
}

const groups = ref<AccountGroup[]>([])
const groupId = ref('')
const accounts = ref<Account[]>([])
const accountsLoading = ref(false)
const search = ref('')
const selected = ref<Set<string>>(new Set())

const model = ref('gpt-6-astra')
const effort = ref('medium')
const prompt = ref(PELICAN_PROMPT)
const concurrency = ref(3)

const running = ref(false)
const runs = ref<RunState[]>([])

const effortOptions = [
  { label: '思考强度：低', value: 'low' },
  { label: '思考强度：中', value: 'medium' },
  { label: '思考强度：高', value: 'high' },
  { label: '思考强度：xhigh', value: 'xhigh' },
  { label: '思考强度：max', value: 'max' },
]
const groupOptions = computed(() => [
  { label: '全部分组', value: '' },
  ...groups.value.map(g => ({ label: `${g.name}`, value: g.id })),
])

const filteredAccounts = computed(() => {
  const kw = search.value.trim().toLowerCase()
  if (!kw)
    return accounts.value
  return accounts.value.filter(a =>
    (a.name || '').toLowerCase().includes(kw)
    || (a.email || '').toLowerCase().includes(kw)
    || a.id.toLowerCase().includes(kw),
  )
})

const allVisibleSelected = computed(() =>
  filteredAccounts.value.length > 0 && filteredAccounts.value.every(a => selected.value.has(a.id)),
)

const CSP = '<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; style-src \'unsafe-inline\'; img-src data:; font-src data:;">'
function previewSrcdoc(out: string) {
  return out ? `${CSP}\n${out}` : ''
}

async function loadGroups() {
  try {
    const res = await getAccountGroups({ page: 1, pageSize: 200 })
    groups.value = res.items ?? []
  }
  catch {
    // 分组加载失败不阻断，账号仍可全量选
  }
}

async function loadAccounts() {
  accountsLoading.value = true
  try {
    const res = await getAccounts({
      page: 1,
      pageSize: 200,
      ...(groupId.value ? { groupId: groupId.value } : {}),
    })
    accounts.value = res.items ?? []
    // 去掉已不在列表里的选择
    const ids = new Set(accounts.value.map(a => a.id))
    selected.value = new Set([...selected.value].filter(id => ids.has(id)))
  }
  catch {
    toast.error('账号列表加载失败')
  }
  finally {
    accountsLoading.value = false
  }
}

function onGroupChange() {
  loadAccounts()
}

function toggle(id: string, on: boolean) {
  const next = new Set(selected.value)
  if (on)
    next.add(id)
  else next.delete(id)
  selected.value = next
}

function toggleAllVisible(on: boolean) {
  const next = new Set(selected.value)
  for (const a of filteredAccounts.value) {
    if (on)
      next.add(a.id)
    else next.delete(a.id)
  }
  selected.value = next
}

function clearSelection() {
  selected.value = new Set()
}

function parseSse(chunk: string, onEvent: (ev: any) => void) {
  for (const line of chunk.split(/\r?\n/)) {
    if (line.startsWith('data:')) {
      try {
        onEvent(JSON.parse(line.slice(5).trim()))
      }
      catch {
        // 忽略非 JSON 行（keep-alive 注释等）
      }
    }
  }
}

async function runOne(run: RunState) {
  run.status = 'running'
  run.output = ''
  run.errorMsg = ''
  const started = performance.now()
  const handle = (ev: { type?: string, text?: string, message?: string, success?: boolean }) => {
    switch (ev.type) {
      case 'content':
        if (typeof ev.text === 'string')
          run.output += ev.text
        break
      case 'test_complete':
        run.status = ev.success === false ? 'error' : (run.output ? 'success' : 'empty')
        if (ev.success === false && !run.errorMsg)
          run.errorMsg = '上游返回失败'
        break
      case 'error':
        run.status = 'error'
        run.errorMsg = ev.message || ev.text || '测试失败'
        break
    }
  }
  try {
    const resp = await fetch(`${API_BASE_URL}/api/admin/accounts/test-bench`, {
      method: 'POST',
      credentials: 'include',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        accountId: run.accountId,
        modelId: model.value.trim(),
        prompt: prompt.value,
        reasoningEffort: effort.value || null,
      }),
    })
    if (!resp.ok || !resp.body) {
      run.status = 'error'
      run.errorMsg = `请求失败：HTTP ${resp.status}`
      return
    }
    const reader = resp.body.getReader()
    const decoder = new TextDecoder()
    let buf = ''
    const sep = /\r?\n\r?\n/
    while (true) {
      const { done, value } = await reader.read()
      if (done)
        break
      buf += decoder.decode(value, { stream: true })
      let m = sep.exec(buf)
      while (m) {
        parseSse(buf.slice(0, m.index), handle)
        buf = buf.slice(m.index + m[0].length)
        m = sep.exec(buf)
      }
    }
    buf += decoder.decode()
    if (buf.trim())
      parseSse(buf, handle)
    if (run.status === 'running')
      run.status = run.output ? 'success' : 'empty'
  }
  catch (error) {
    run.status = 'error'
    run.errorMsg = error instanceof Error ? error.message : '网络错误'
  }
  finally {
    run.ms = Math.round(performance.now() - started)
  }
}

async function runBatch() {
  const ids = accounts.value.filter(a => selected.value.has(a.id)).map(a => a.id)
  if (!ids.length) {
    toast.error('请先勾选至少一个账号')
    return
  }
  if (!prompt.value.trim()) {
    toast.error('请填写测试 prompt')
    return
  }
  const nameOf = (id: string) => accounts.value.find(a => a.id === id)?.name || id
  runs.value = ids.map(id => ({
    accountId: id,
    name: nameOf(id),
    status: 'queued' as RunStatus,
    output: '',
    errorMsg: '',
    verdict: 'unknown' as Verdict,
    note: '',
    ms: 0,
  }))
  running.value = true
  const limit = Math.max(1, Math.min(concurrency.value || 1, 8))
  let cursor = 0
  const worker = async () => {
    while (cursor < runs.value.length) {
      const idx = cursor++
      await runOne(runs.value[idx])
    }
  }
  try {
    await Promise.all(Array.from({ length: Math.min(limit, runs.value.length) }, () => worker()))
  }
  finally {
    running.value = false
  }
}

function setVerdict(run: RunState, v: Verdict) {
  run.verdict = run.verdict === v ? 'unknown' : v
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

onMounted(() => {
  loadGroups()
  loadAccounts()
})
</script>

<template>
  <div class="flex w-full flex-col gap-5 px-4 py-6">
    <BasePageHeader
      title="测智台"
      description="选一批账号并行发同一条测试 prompt（可选思考强度），并排看输出、人工判满血/降智。走 probe 路径钉住账号，钉票随账号自动带。" />

    <BaseCard>
      <div class="flex flex-col gap-4">
        <!-- 参数行 -->
        <div class="flex flex-wrap items-end gap-3">
          <div class="w-48">
            <label class="block text-xs text-neutral-500">分组</label>
            <BaseSelect v-model="groupId" :options="groupOptions" class="mt-1" @update:model-value="onGroupChange" />
          </div>
          <div class="w-48">
            <label class="block text-xs text-neutral-500">模型</label>
            <BaseSelect
              v-model="model" class="mt-1"
              :options="[
                { label: 'gpt-6-astra', value: 'gpt-6-astra' },
                { label: 'gpt-6-sol', value: 'gpt-6-sol' },
                { label: 'gpt-5.6-sol', value: 'gpt-5.6-sol' },
                { label: 'gpt-5.5', value: 'gpt-5.5' },
              ]" />
          </div>
          <div class="w-40">
            <label class="block text-xs text-neutral-500">思考强度</label>
            <BaseSelect v-model="effort" :options="effortOptions" class="mt-1" />
          </div>
          <div class="w-32">
            <label class="block text-xs text-neutral-500">并发上限</label>
            <BaseNumberInput v-model="concurrency" label="并发上限" :min="1" :max="8" class="mt-1" />
          </div>
          <BaseButton :loading="running" :disabled="running || selected.size === 0" @click="runBatch">
            开始测试（{{ selected.size }}）
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
              <span v-else class="ml-1 text-neutral-400">（{{ filteredAccounts.length }} 个，已选 {{ selected.size }}）</span>
            </label>
            <BaseInput v-model="search" placeholder="搜索名称/邮箱/ID" class="h-7 w-56 text-xs" />
            <BaseCheckbox
              :model-value="allVisibleSelected" label="全选可见" show-label
              class="text-xs" @update:model-value="toggleAllVisible" />
            <button class="text-xs text-neutral-500 hover:text-neutral-800 dark:hover:text-neutral-200" @click="clearSelection">
              清空
            </button>
          </div>
          <div class="max-h-64 overflow-auto rounded-md border border-neutral-200 dark:border-neutral-800">
            <div
              v-for="a in filteredAccounts" :key="a.id"
              class="flex items-center gap-2 border-b border-neutral-100 px-3 py-1.5 text-sm last:border-b-0 dark:border-neutral-800/60">
              <BaseCheckbox
                :model-value="selected.has(a.id)" :label="a.name || a.id"
                @update:model-value="(v: boolean) => toggle(a.id, v)" />
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
      <BaseCard v-for="run in runs" :key="run.accountId" class="flex flex-col">
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
            <!-- 人工判定（独立于运行状态） -->
            <div class="flex items-center gap-1.5">
              <button
                class="rounded px-2 py-0.5 text-[11px] ring-1 transition"
                :class="run.verdict === 'full' ? 'bg-emerald-500/15 text-emerald-500 ring-emerald-500/40' : 'text-neutral-500 ring-neutral-300 hover:ring-emerald-400 dark:ring-neutral-700'"
                @click="setVerdict(run, 'full')">
                满血
              </button>
              <button
                class="rounded px-2 py-0.5 text-[11px] ring-1 transition"
                :class="run.verdict === 'degraded' ? 'bg-rose-500/15 text-rose-500 ring-rose-500/40' : 'text-neutral-500 ring-neutral-300 hover:ring-rose-400 dark:ring-neutral-700'"
                @click="setVerdict(run, 'degraded')">
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
