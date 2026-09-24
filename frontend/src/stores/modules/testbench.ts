// 测智台状态放 store：切走页面再回来，上一次的结果、进行中的生成都还在（收流循环挂在 store 上，
// 不随页面卸载中断）。表单配置持久到 localStorage；结果持久到 sessionStorage（刷新不丢已完成的结果）。
import type { Account, AccountGroup } from '@/api'
import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { getAccountGroups, getAccounts } from '@/api'
import { API_BASE_URL } from '@/api/constants'
import { toast } from '@/components/base/BaseToast'

export const PELICAN_PROMPT
  = 'Create an HTML page with an SVG drawing of a pelican riding a bicycle in 2D. Output only the HTML, no explanation, no markdown fences.'

const RUNS_STORAGE_KEY = 'codex-proxy-rs-testbench-runs'

function loadRuns(): RunState[] {
  try {
    const raw = sessionStorage.getItem(RUNS_STORAGE_KEY)
    if (!raw)
      return []
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? parsed : []
  }
  catch {
    return []
  }
}

export type RunStatus = 'queued' | 'running' | 'success' | 'empty' | 'error'
export type Verdict = 'unknown' | 'full' | 'degraded'

export interface RunState {
  accountId: string
  name: string
  model: string
  effort: string
  status: RunStatus
  output: string
  errorMsg: string
  verdict: Verdict
  note: string
  ms: number
  startedAt: string
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

export const useTestBenchStore = defineStore(
  'testbench',
  () => {
    // 表单（持久到 localStorage）
    const groupId = ref('')
    const model = ref('gpt-6-astra')
    const effort = ref('medium')
    const prompt = ref(PELICAN_PROMPT)
    const concurrency = ref(3)
    const selected = ref<string[]>([])

    // 目录（不持久，进页面时按需加载）
    const groups = ref<AccountGroup[]>([])
    const accounts = ref<Account[]>([])
    const accountsLoading = ref(false)
    const search = ref('')

    // 结果（手动持久到 sessionStorage：只在开跑/单份结束/人工判定时写，不跟着每个 SSE 片段写）
    // 与运行态（不持久）
    const runs = ref<RunState[]>(loadRuns())
    const running = ref(false)

    function persistRuns() {
      try {
        sessionStorage.setItem(RUNS_STORAGE_KEY, JSON.stringify(runs.value))
      }
      catch {
        // 配额/隐私模式下写不进去就只留内存
      }
    }

    const selectedSet = computed(() => new Set(selected.value))
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
      filteredAccounts.value.length > 0 && filteredAccounts.value.every(a => selectedSet.value.has(a.id)),
    )

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
        selected.value = selected.value.filter(id => ids.has(id))
      }
      catch {
        toast.error('账号列表加载失败')
      }
      finally {
        accountsLoading.value = false
      }
    }

    /// 进页面时补齐目录：store 里已有就不重复拉。
    async function ensureLoaded() {
      const jobs: Promise<void>[] = []
      if (!groups.value.length)
        jobs.push(loadGroups())
      if (!accounts.value.length)
        jobs.push(loadAccounts())
      await Promise.all(jobs)
    }

    function toggle(id: string, on: boolean) {
      const next = new Set(selected.value)
      if (on)
        next.add(id)
      else next.delete(id)
      selected.value = [...next]
    }

    function toggleAllVisible(on: boolean) {
      const next = new Set(selected.value)
      for (const a of filteredAccounts.value) {
        if (on)
          next.add(a.id)
        else next.delete(a.id)
      }
      selected.value = [...next]
    }

    function clearSelection() {
      selected.value = []
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
            modelId: run.model,
            prompt: prompt.value,
            reasoningEffort: run.effort || null,
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
        persistRuns()
      }
    }

    async function runBatch() {
      if (running.value)
        return
      const ids = accounts.value.filter(a => selectedSet.value.has(a.id)).map(a => a.id)
      if (!ids.length) {
        toast.error('请先勾选至少一个账号')
        return
      }
      if (!prompt.value.trim()) {
        toast.error('请填写测试 prompt')
        return
      }
      const nameOf = (id: string) => accounts.value.find(a => a.id === id)?.name || id
      const modelId = model.value.trim()
      const effortValue = effort.value
      runs.value = ids.map(id => ({
        accountId: id,
        name: nameOf(id),
        model: modelId,
        effort: effortValue,
        status: 'queued' as RunStatus,
        output: '',
        errorMsg: '',
        verdict: 'unknown' as Verdict,
        note: '',
        ms: 0,
        startedAt: new Date().toISOString(),
      }))
      running.value = true
      persistRuns()
      const limit = Math.max(1, Math.min(concurrency.value || 1, 8))
      let cursor = 0
      const batch = runs.value
      const worker = async () => {
        while (cursor < batch.length) {
          const idx = cursor++
          await runOne(batch[idx])
        }
      }
      try {
        await Promise.all(Array.from({ length: Math.min(limit, batch.length) }, () => worker()))
      }
      finally {
        running.value = false
      }
    }

    function setVerdict(run: RunState, v: Verdict) {
      run.verdict = run.verdict === v ? 'unknown' : v
      persistRuns()
    }

    function clearRuns() {
      if (running.value)
        return
      runs.value = []
      persistRuns()
    }

    /// 刷新页面后 sessionStorage 恢复的结果里，仍标着「生成中/排队」的其实已经断了。
    function reconcileInterrupted() {
      if (running.value)
        return
      for (const run of runs.value) {
        if (run.status === 'running' || run.status === 'queued') {
          run.status = run.output ? 'success' : 'error'
          if (!run.output)
            run.errorMsg = '页面刷新，本次测试中断'
        }
      }
      persistRuns()
    }

    return {
      groupId,
      model,
      effort,
      prompt,
      concurrency,
      selected,
      groups,
      accounts,
      accountsLoading,
      search,
      runs,
      running,
      selectedSet,
      filteredAccounts,
      allVisibleSelected,
      loadGroups,
      loadAccounts,
      ensureLoaded,
      toggle,
      toggleAllVisible,
      clearSelection,
      runBatch,
      setVerdict,
      clearRuns,
      reconcileInterrupted,
    }
  },
  {
    persist: {
      key: 'codex-proxy-rs-testbench-form',
      pick: ['groupId', 'model', 'effort', 'prompt', 'concurrency', 'selected'],
    },
  },
)
