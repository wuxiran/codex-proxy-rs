<script setup lang="ts">
// 测智台：对指定账号发一条自定义 prompt（可选思考强度）的测试请求，看输出质量（人工判满血/降智）。
// 走后台 probe 路径钉住账号，钉票随账号自动带。SSE 用 fetch 流式读取（POST 带 prompt）。
// 输出（常是 HTML/SVG）用 sandbox="" iframe 隔离预览：无脚本/无同源/无表单/无导航 + CSP 断远程资源。
import type { Account } from '@/api'
import { computed, onMounted, ref } from 'vue'
import { getAccounts } from '@/api'
import { API_BASE_URL } from '@/api/constants'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { toast } from '@/components/base/BaseToast'

const PELICAN_PROMPT
  = 'Create an HTML page with an SVG drawing of a pelican riding a bicycle in 2D. Output only the HTML, no explanation, no markdown fences.'

const accounts = ref<Account[]>([])
const accountsLoading = ref(false)
const accountId = ref('')
const model = ref('gpt-6-astra')
const effort = ref('medium')
const prompt = ref(PELICAN_PROMPT)

const running = ref(false)
const output = ref('')
const status = ref<'idle' | 'running' | 'success' | 'error'>('idle')
const errorMsg = ref('')

const effortOptions = [
  { label: '思考强度：低', value: 'low' },
  { label: '思考强度：中', value: 'medium' },
  { label: '思考强度：高', value: 'high' },
  { label: '思考强度：xhigh', value: 'xhigh' },
  { label: '思考强度：max', value: 'max' },
]
const accountOptions = computed(() =>
  accounts.value.map(a => ({ label: `${a.name || a.id}（${a.provider}${a.enabled ? '' : ' · 停用'}）`, value: a.id })),
)

// sandbox="" 已禁脚本/同源；再加 CSP 断远程资源（只允许 data: 图片与内联样式）。
const previewSrcdoc = computed(() => {
  const csp = '<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; style-src \'unsafe-inline\'; img-src data:; font-src data:;">'
  return output.value ? `${csp}\n${output.value}` : ''
})

async function loadAccounts() {
  accountsLoading.value = true
  try {
    const res = await getAccounts({ page: 1, pageSize: 200 })
    accounts.value = res.items ?? []
    if (!accountId.value && accounts.value.length)
      accountId.value = accounts.value[0].id
  }
  catch {
    toast.error('账号列表加载失败')
  }
  finally {
    accountsLoading.value = false
  }
}

onMounted(loadAccounts)

function handleEvent(ev: { type?: string, text?: string, message?: string, success?: boolean }) {
  switch (ev.type) {
    case 'content':
      if (typeof ev.text === 'string')
        output.value += ev.text
      break
    case 'test_complete':
      status.value = ev.success === false ? 'error' : 'success'
      if (ev.success === false && !errorMsg.value)
        errorMsg.value = '上游返回失败'
      break
    case 'error':
      status.value = 'error'
      errorMsg.value = ev.message || ev.text || '测试失败'
      break
    default:
      // test_start / request / status 等：忽略
      break
  }
}

async function runTest() {
  if (!accountId.value || !prompt.value.trim()) {
    toast.error('请选择账号并填写 prompt')
    return
  }
  running.value = true
  status.value = 'running'
  output.value = ''
  errorMsg.value = ''
  try {
    const resp = await fetch(`${API_BASE_URL}/api/admin/accounts/test-bench`, {
      method: 'POST',
      credentials: 'include',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        accountId: accountId.value,
        modelId: model.value.trim(),
        prompt: prompt.value,
        reasoningEffort: effort.value || null,
      }),
    })
    if (!resp.ok || !resp.body) {
      status.value = 'error'
      errorMsg.value = `请求失败：HTTP ${resp.status}`
      return
    }
    const reader = resp.body.getReader()
    const decoder = new TextDecoder()
    let buf = ''
    const flush = (chunk: string) => {
      for (const line of chunk.split(/\r?\n/)) {
        if (line.startsWith('data:')) {
          try {
            handleEvent(JSON.parse(line.slice(5).trim()))
          }
          catch {
            // 忽略非 JSON 行（keep-alive 注释等）
          }
        }
      }
    }
    // 事件以空行分隔，兼容 \n\n 与 \r\n\r\n。
    const sep = /\r?\n\r?\n/
    while (true) {
      const { done, value } = await reader.read()
      if (done)
        break
      buf += decoder.decode(value, { stream: true })
      let m = sep.exec(buf)
      while (m) {
        flush(buf.slice(0, m.index))
        buf = buf.slice(m.index + m[0].length)
        m = sep.exec(buf)
      }
    }
    buf += decoder.decode()
    if (buf.trim())
      flush(buf)
    if (status.value === 'running')
      status.value = 'success'
  }
  catch (error) {
    status.value = 'error'
    errorMsg.value = error instanceof Error ? error.message : '网络错误'
  }
  finally {
    running.value = false
  }
}

function downloadOutput() {
  const blob = new Blob([output.value], { type: 'text/html;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `testbench-${accountId.value || 'output'}.html`
  a.click()
  URL.revokeObjectURL(url)
}
</script>

<template>
  <div class="flex w-full flex-col gap-5 px-4 py-6">
    <BasePageHeader
      title="测智台"
      description="对指定账号发一条测试 prompt（可选思考强度），并排看输出质量、人工判满血/降智。走 probe 路径钉住账号，钉票随账号自动带。" />

    <BaseCard>
      <div class="flex flex-col gap-3">
        <div class="flex flex-wrap items-end gap-3">
          <div class="min-w-60 flex-1">
            <label class="block text-xs text-neutral-500">
              账号<span v-if="accountsLoading" class="ml-1 text-amber-500">加载中…</span>
              <span v-else class="ml-1 text-neutral-400">（{{ accounts.length }}）</span>
            </label>
            <BaseSelect v-model="accountId" :options="accountOptions" class="mt-1" placeholder="选择账号" />
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
          <div class="w-44">
            <label class="block text-xs text-neutral-500">思考强度</label>
            <BaseSelect v-model="effort" :options="effortOptions" class="mt-1" />
          </div>
          <BaseButton :loading="running" :disabled="running" @click="runTest">
            开始测试
          </BaseButton>
        </div>
        <div>
          <label class="block text-xs text-neutral-500">Prompt</label>
          <BaseTextarea v-model="prompt" :rows="4" class="mt-1" placeholder="输入测试 prompt" />
        </div>
      </div>
    </BaseCard>

    <BaseCard v-if="status !== 'idle'">
      <template #header>
        <div class="flex items-center justify-between">
          <div class="text-sm font-semibold">
            输出
            <span
              class="ml-2 rounded-full border px-2 py-0.5 text-[11px]"
              :class="status === 'success' ? 'border-emerald-500/40 text-emerald-500'
                : status === 'error' ? 'border-rose-500/40 text-rose-500'
                  : 'border-amber-500/40 text-amber-500'">
              {{ status === 'running' ? '生成中…' : status === 'success' ? '完成' : '失败' }}
            </span>
          </div>
          <BaseButton v-if="output" size="sm" variant="secondary" @click="downloadOutput">
            下载 HTML
          </BaseButton>
        </div>
      </template>
      <template #body>
        <div v-if="errorMsg" class="mb-3 rounded-md border border-rose-500/30 bg-rose-500/5 px-3 py-2 text-xs text-rose-500">
          {{ errorMsg }}
        </div>
        <div v-if="output" class="grid gap-4 lg:grid-cols-2">
          <div>
            <div class="mb-1 text-xs text-neutral-500">预览（隔离沙箱）</div>
            <iframe
              :srcdoc="previewSrcdoc"
              sandbox=""
              class="h-[520px] w-full rounded-md border border-neutral-200 bg-white dark:border-neutral-800" />
          </div>
          <div>
            <div class="mb-1 text-xs text-neutral-500">原文（{{ output.length }} 字符）</div>
            <pre class="h-[520px] overflow-auto rounded-md border border-neutral-200 bg-neutral-50 p-3 text-[11px] leading-relaxed dark:border-neutral-800 dark:bg-neutral-900">{{ output }}</pre>
          </div>
        </div>
        <div v-else-if="status === 'running'" class="text-sm text-neutral-500">
          等待输出…
        </div>
        <div v-else-if="status === 'success'" class="text-sm text-neutral-500">
          测试完成，但上游未返回任何文本内容（可能被拒绝或空响应）。
        </div>
      </template>
    </BaseCard>
  </div>
</template>
