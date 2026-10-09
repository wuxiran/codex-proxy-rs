<script setup lang="ts">
// 常用流程只展示专用打票代理、有效票据和模型声明，低频诊断折叠展示。
import type {
  TurnStateBucket,
  TurnStateBucketTally,
  TurnStateInjectMode,
  TurnStateLengthClass,
  TurnStateObservations,
  TurnStateServedMismatchAction,
  TurnStateSettings,
} from '@/api'
import { BaseButton, BaseCard, BaseCheckbox, BaseInput, BaseNumberInput, BasePageHeader, BaseSelect, toast } from '@codex-proxy/ui'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  clearTurnStateBucket,
  getTurnStateBuckets,
  getTurnStateObservations,
  getTurnStateSettings,
  updateTurnStateSettings,
} from '@/api'
import SyncedSwitch from '@/components/SyncedSwitch.vue'
import AccountTicketManager from './components/AccountTicketManager.vue'
import PoolOverview from './components/PoolOverview.vue'
import { usePoolOverview } from './composables/usePoolOverview'

const pool = usePoolOverview()

function refreshRuntime() {
  void loadData()
  void pool.refresh()
}

const MIN_LEN = 200
const POLL_MS = 30_000

const settings = ref<TurnStateSettings | null>(null)
const settingsDirty = ref(false)
const saving = ref(false)
const buckets = ref<TurnStateBucket[]>([])
const bucketsNow = ref(0)
const observations = ref<TurnStateObservations | null>(null)
const loading = ref(false)
const settingsError = ref('')
const newTemplateLen = ref(MIN_LEN)
const newDegradedLen = ref(MIN_LEN)

const mintModeOptions = [
  { label: '专用代理', value: 'native' },
  { label: '中继服务', value: 'relay' },
]
const transportOptions = [
  { label: 'SSE', value: 'sse' },
  { label: 'WebSocket', value: 'websocket' },
]
const effortOptions = [
  { label: 'low', value: 'low' },
  { label: 'medium', value: 'medium' },
  { label: 'high', value: 'high' },
  { label: 'xhigh', value: 'xhigh' },
]
const warmModelsText = computed({
  get: () => settings.value?.warmPool.probeModel || settings.value?.warmPool.models.join(', ') || '',
  set: (value: string) => {
    if (settings.value) {
      settings.value.warmPool.models = value.split(/[\s,]+/).map(item => item.trim()).filter(Boolean)
      settings.value.warmPool.probeModel = ''
      markDirty()
    }
  },
})
const mintModelsText = computed({
  get: () => settings.value?.cloudMint.models.join(', ') ?? '',
  set: (value: string) => {
    if (!settings.value)
      return
    settings.value.cloudMint.models = value.split(/[\s,]+/).map(item => item.trim()).filter(Boolean)
    markDirty()
  },
})
const relayKeyInput = computed({
  get: () => settings.value?.cloudMint.relayKey ?? '',
  set: (value: string) => {
    if (!settings.value)
      return
    settings.value.cloudMint.relayKey = value
    markDirty()
  },
})

// 密钥字段读取只拿占位符，输入为空时仍保留已保存的值，清除必须点独立按钮。
const mintProxyInput = computed({
  get: () => settings.value?.cloudMint.upstreamProxyUrl === '<set>' ? '' : settings.value?.cloudMint.upstreamProxyUrl ?? '',
  set: (value: string) => {
    if (settings.value) {
      settings.value.cloudMint.upstreamProxyUrl = value
      markDirty()
    }
  },
})
const hasMintProxy = computed(() => Boolean(settings.value?.cloudMint.upstreamProxyUrl?.trim()))
const proxyError = computed(() => {
  const mint = settings.value?.cloudMint
  if (!mint || mint.mode !== 'native')
    return ''
  const raw = mint.upstreamProxyUrl?.trim() ?? ''
  if (!raw)
    return mint.enabled ? '请先填写专用打票代理' : ''
  if (raw === '<set>')
    return ''
  try {
    const url = new URL(raw)
    if (['http:', 'https:', 'socks5:', 'socks5h:'].includes(url.protocol) && url.hostname && url.port !== '0' && (url.port || ['http:', 'https:'].includes(url.protocol)) && ['', '/'].includes(url.pathname) && !url.search && !url.hash)
      return ''
  }
  catch {}
  return '请填写完整的 HTTP、HTTPS 或 SOCKS5 代理地址'
})
const warmError = computed(() => {
  const warm = settings.value?.warmPool
  if (!warm?.enabled)
    return ''
  if (warm.requireVerified && (!warm.probe || !warm.businessReuse))
    return '只用已验证连接需要开启业务复用和答案检查'
  if (warm.probe && !warm.probeExpect.trim())
    return '请填写验证通过判据'
  if (warm.models.length > 16)
    return '预热模型最多填写 16 个'
  return ''
})
const canSave = computed(() => settingsDirty.value && !saving.value && !proxyError.value && !warmError.value)
const activeBuckets = computed(() => buckets.value.filter(bucket => bucket.expiresAt > bucketsNow.value))
const mismatchCount = computed(() => Object.values(observations.value?.buckets ?? {}).reduce((sum, row) => sum + (row.servedMismatch ?? 0), 0))

function clearMintProxy() {
  if (settings.value) {
    settings.value.cloudMint.upstreamProxyUrl = ''
    markDirty()
  }
}

const servedMismatchOptions: Array<{ label: string, value: TurnStateServedMismatchAction }> = [
  { label: '记录并返回响应', value: 'observe' },
  { label: '阻断响应，不自动重试', value: 'block' },
]

const injectModeOptions: Array<{ label: string, value: TurnStateInjectMode }> = [
  { label: '缺票时补充，保留客户端票', value: 'fill-missing' },
  { label: '始终使用已保存票据', value: 'always' },
  { label: '按自定义长度替换', value: 'replace-only' },
]

async function loadSettings() {
  try {
    settingsError.value = ''
    settings.value = await getTurnStateSettings({ silent: true })
    settings.value.cloudMint.upstreamProxyUrl ??= ''
    settingsDirty.value = false
  }
  catch {
    settingsError.value = '设置读取失败，请重试'
  }
}

async function loadData() {
  loading.value = true
  try {
    const [bucketRes, obs] = await Promise.all([
      getTurnStateBuckets({}, { silent: true }),
      getTurnStateObservations({ silent: true }),
    ])
    buckets.value = bucketRes.buckets
    bucketsNow.value = bucketRes.now
    observations.value = obs
  }
  catch {
    // 轮询失败不打扰；下一轮再试
  }
  finally {
    loading.value = false
  }
}

async function saveSettings() {
  if (!settings.value || proxyError.value || warmError.value)
    return
  saving.value = true
  try {
    settings.value = await updateTurnStateSettings(settings.value)
    settingsDirty.value = false
    toast.success('设置已保存')
  }
  catch (error) {
    toast.error(error instanceof Error ? error.message : '保存失败')
  }
  finally {
    saving.value = false
  }
}

function markDirty() {
  settingsDirty.value = true
}

function addLength(kind: 'template' | 'degraded', len: number) {
  if (!settings.value || !Number.isInteger(len) || len < MIN_LEN)
    return
  const own = kind === 'template' ? settings.value.templateLengths : settings.value.degradedLengths
  const other = kind === 'template' ? settings.value.degradedLengths : settings.value.templateLengths
  if (other.includes(len)) {
    toast.error(`${len} 已在${kind === 'template' ? '受限' : '模板'}表里，两表不能重叠`)
    return
  }
  if (!own.includes(len)) {
    own.push(len)
    own.sort((a, b) => a - b)
    markDirty()
  }
}

function removeLength(kind: 'template' | 'degraded', len: number) {
  if (!settings.value)
    return
  const list = kind === 'template' ? settings.value.templateLengths : settings.value.degradedLengths
  const index = list.indexOf(len)
  if (index >= 0) {
    list.splice(index, 1)
    markDirty()
  }
}

async function clearBucket(bucket: TurnStateBucket) {
  try {
    const res = await clearTurnStateBucket({ account: bucket.account, model: bucket.model })
    toast.success(`已清除 ${res.cleared} 条`)
    await loadData()
  }
  catch {
    toast.error('清除失败')
  }
}

function classify(len: number): TurnStateLengthClass {
  const s = settings.value
  if (!s)
    return 'unknown'
  if (s.degradedLengths.includes(len))
    return 'degraded'
  if (s.templateLengths.length === 0)
    return len >= MIN_LEN ? 'normal' : 'unknown'
  return s.templateLengths.includes(len) ? 'normal' : 'unknown'
}

const histogram = computed(() => {
  const raw = observations.value?.histogram ?? {}
  const total = Object.values(raw).reduce((sum, n) => sum + n, 0)
  return Object.entries(raw)
    .map(([len, count]) => ({ len: Number(len), count, share: total ? count / total : 0, cls: classify(Number(len)) }))
    .sort((a, b) => b.count - a.count)
})

interface TallyRow {
  key: string
  account: string
  model: string
  tally: TurnStateBucketTally
}

const tallyRows = computed<TallyRow[]>(() => {
  const raw = observations.value?.buckets ?? {}
  return Object.entries(raw).map(([key, tally]) => {
    const slash = key.indexOf('/')
    const account = slash >= 0 ? key.slice(0, slash) : key
    const model = slash >= 0 ? key.slice(slash + 1) : ''
    return { key, account, model, tally }
  }).sort((a, b) => b.tally.lastSeenAt - a.tally.lastSeenAt)
})

const hourly = computed(() => {
  const rows = [...(observations.value?.hourly ?? [])].sort((a, b) => a.hour - b.hour)
  const max = Math.max(1, ...rows.map(r => r.normal + r.degraded + r.unknown + r.silent))
  return rows.map(r => ({ ...r, max }))
})

const events = computed(() => observations.value?.events ?? [])

function fmtTime(secs: number) {
  if (!secs)
    return '—'
  return new Date(secs * 1000).toLocaleString()
}

function fmtRemaining(expiresAt: number) {
  const delta = expiresAt - (bucketsNow.value || Math.floor(Date.now() / 1000))
  if (delta <= 0)
    return '已过期'
  if (delta < 60)
    return `${Math.floor(delta)} 秒`
  const minutes = Math.floor(delta / 60)
  return minutes >= 60 ? `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分` : `${minutes} 分`
}

const sourceText: Record<TurnStateBucket['source'], string> = {
  hunt: '遍历命中',
  passive: '被动捕获',
  renewal: '按需补票',
  mint: '专用打票',
}

const decisionText: Record<string, string> = {
  inject: '注入',
  substitute: '替换',
  pass: '放行',
  skip: '跳过',
  harvest: '采集',
}

let timer: ReturnType<typeof setInterval> | undefined
onMounted(async () => {
  await Promise.all([loadSettings(), loadData()])
  timer = setInterval(loadData, POLL_MS)
})
onBeforeUnmount(() => {
  if (timer)
    clearInterval(timer)
})
</script>

<template>
  <div class="flex w-full flex-col gap-5 py-6 pr-4 pl-6 sm:px-4">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <BasePageHeader title="票据管理" description="管理账号票据、预热连接和打票代理" />
      <div class="flex items-center gap-3">
        <span v-if="settingsDirty" class="text-cp-sm text-cp-text-secondary">有未保存的修改</span>
        <BaseButton variant="primary" :loading="saving" :disabled="!canSave" @click="saveSettings">
          保存设置
        </BaseButton>
      </div>
    </div>

    <div v-if="settingsError" class="flex items-center gap-3 text-cp-sm text-cp-danger" role="alert">
      <span>{{ settingsError }}</span>
      <BaseButton size="sm" @click="loadSettings">
        重新读取
      </BaseButton>
    </div>
    <p v-else-if="!settings" class="text-cp-sm text-cp-text-secondary" role="status">
      正在读取设置
    </p>

    <PoolOverview :snapshot="pool.snapshot.value" :loading="pool.loading.value" :stale="pool.stale.value" :paused="pool.paused.value" :error="pool.error.value" :now="pool.now.value" @refresh="pool.refresh" />
    <AccountTicketManager :pool-accounts="pool.accounts.value" :pool-stale="pool.stale.value" :pool-paused="pool.paused.value" :pool-now="pool.now.value" @changed="refreshRuntime" />

    <BaseCard v-if="settings" title="自动打票" description="用于已开启「票与预热」的账号">
      <div class="flex flex-col gap-4">
        <div class="flex flex-wrap items-center gap-5">
          <SyncedSwitch v-model="settings.cloudMint.enabled" label="缺票时自动补充" show-label @update:model-value="markDirty" />
          <span class="text-cp-sm text-cp-text-secondary">{{ settings.cloudMint.mode === 'native' ? (hasMintProxy ? '专用代理已配置' : '尚未配置打票代理') : '使用中继服务' }}</span>
          <span v-if="settings.dryRun" class="text-cp-sm text-cp-warning">模拟运行中，不会打票或改写请求</span>
        </div>
        <div class="grid gap-4 md:grid-cols-2">
          <div v-if="settings.cloudMint.mode === 'native'" class="min-w-0">
            <span id="mint-proxy-label" class="mb-1 block text-cp-sm text-cp-text">专用打票代理</span>
            <div class="flex items-center gap-2">
              <BaseInput id="mint-proxy" v-model="mintProxyInput" type="password" autocomplete="new-password" aria-label="专用打票代理" aria-labelledby="mint-proxy-label" :placeholder="settings.cloudMint.upstreamProxyUrl === '<set>' ? '已保存，输入新地址可更换' : 'socks5h://用户名:密码@地址:端口'" class="min-w-0 flex-1" />
              <BaseButton v-if="hasMintProxy" size="sm" variant="soft" @click="clearMintProxy">
                清除
              </BaseButton>
            </div>
            <p v-if="proxyError" class="mt-1 text-cp-sm text-cp-danger" role="alert">
              {{ proxyError }}
            </p>
            <p v-else class="mt-1 text-cp-xs text-cp-text-secondary">
              用于打票和连接预热，通过验证的连接可供业务复用
            </p>
          </div>
          <div>
            <span id="mint-models-label" class="mb-1 block text-cp-sm text-cp-text">打票模型</span>
            <BaseInput id="mint-models" v-model="mintModelsText" aria-label="打票模型" aria-labelledby="mint-models-label" placeholder="留空时跟随业务请求" />
          </div>
        </div>
        <details class="text-cp-sm text-cp-text-secondary">
          <summary class="cursor-pointer py-1">
            高级打票设置
          </summary>
          <div class="mt-3 grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
            <div><span class="mb-1 block">打票方式</span><BaseSelect v-model="settings.cloudMint.mode" :options="mintModeOptions" aria-label="打票方式" @update:model-value="markDirty" /></div>
            <template v-if="settings.cloudMint.mode === 'relay'">
              <div><span class="mb-1 block">中继地址</span><BaseInput v-model="settings.cloudMint.relayUrl" placeholder="https://relay.example.com" aria-label="中继地址" @update:model-value="markDirty" /></div>
              <div><span class="mb-1 block">中继密钥</span><BaseInput v-model="relayKeyInput" type="password" autocomplete="new-password" aria-label="中继密钥" /></div>
              <div><span class="mb-1 block">连接中继的代理</span><BaseInput v-model="settings.cloudMint.proxyUrl" type="password" aria-label="连接中继的代理" placeholder="留空时直接连接中继" @update:model-value="markDirty" /></div>
              <div><span class="mb-1 block">中继打票协议</span><BaseSelect v-model="settings.cloudMint.transport" :options="transportOptions" @update:model-value="markDirty" /></div>
              <p class="col-span-full m-0">
                中继到上游的出口由中继服务配置，专用打票代理用于本机打票
              </p>
            </template>
            <div><span class="mb-1 block">票据有效期（秒）</span><BaseNumberInput v-model="settings.cloudMint.ticketTtlSeconds" label="票据有效期" :min="30" :max="86400" :step="30" @update:model-value="markDirty" /></div>
            <div><span class="mb-1 block">每模型尝试上限</span><BaseNumberInput v-model="settings.cloudMint.maxAttempts" label="每模型尝试上限" :min="1" :max="24" @update:model-value="markDirty" /></div>
            <div><span class="mb-1 block">失败后等待（秒）</span><BaseNumberInput v-model="settings.cloudMint.cooldownSeconds" label="失败后等待" :min="5" :max="3600" @update:model-value="markDirty" /></div>
            <div><span class="mb-1 block">目标节点</span><BaseInput v-model="settings.cloudMint.gateway" placeholder="留空时不限节点" aria-label="目标节点" @update:model-value="markDirty" /></div>
            <div><span class="mb-1 block">预期票据长度</span><BaseNumberInput v-model="settings.cloudMint.ticketLen" label="预期票据长度" :min="0" :max="4096" @update:model-value="markDirty" /></div>
            <BaseCheckbox v-model="settings.cloudMint.observeOnly" label="只验证打票，不应用结果" show-label @update:model-value="markDirty" />
          </div>
        </details>
      </div>
    </BaseCard>

    <BaseCard v-if="settings" title="响应处理">
      <div class="flex flex-wrap items-center gap-4">
        <div class="w-full sm:w-72">
          <BaseSelect v-model="settings.servedMismatchAction" :options="servedMismatchOptions" aria-label="模型不一致时" @update:model-value="markDirty" />
        </div>
        <BaseCheckbox v-model="settings.dryRun" label="模拟运行" show-label @update:model-value="markDirty" />
        <span class="text-cp-sm text-cp-text-secondary">累计 {{ mismatchCount }} 次模型声明不一致</span>
      </div>
    </BaseCard>
    <BaseCard title="有效票据" :description="`共 ${activeBuckets.length} 条`">
      <div class="overflow-auto">
        <table class="w-full text-sm whitespace-nowrap">
          <thead class="text-left text-xs text-neutral-500">
            <tr>
              <th class="px-2 py-1.5">
                账号
              </th>
              <th class="px-2 py-1.5">
                模型
              </th>
              <th class="px-2 py-1.5">
                剩余
              </th>
              <th class="px-2 py-1.5">
                来源
              </th>
              <th class="px-2 py-1.5" />
            </tr>
          </thead>
          <tbody>
            <tr v-for="b in activeBuckets" :key="`${b.account}/${b.model}/${b.scope}/${b.hits}`" class="border-t border-neutral-100 dark:border-neutral-800/60">
              <td class="max-w-56 truncate px-2 py-1.5 font-mono text-xs" :title="b.account">
                {{ b.account }}
              </td>
              <td class="px-2 py-1.5">
                {{ b.model }}
              </td>

              <td class="px-2 py-1.5 text-xs">
                {{ fmtRemaining(b.expiresAt) }}
              </td>
              <td class="px-2 py-1.5 text-xs">
                {{ sourceText[b.source] }}
              </td>
              <td class="px-2 py-1.5 text-right">
                <BaseButton v-if="b.scope === 'account'" size="sm" variant="soft" @click="clearBucket(b)">
                  清除
                </BaseButton>
              </td>
            </tr>
            <tr v-if="!activeBuckets.length">
              <td colspan="5" class="px-2 py-6 text-center text-sm whitespace-normal text-neutral-400">
                暂无有效票据，可在上方选择账号立即打票，或启用自动补票
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </BaseCard>
    <BaseCard title="模型声明">
      <div class="overflow-x-auto">
        <table class="w-full text-left text-cp-sm whitespace-nowrap">
          <thead class="text-cp-text-secondary">
            <tr>
              <th class="p-2">
                账号
              </th><th class="p-2">
                请求模型
              </th><th class="p-2">
                一致
              </th><th class="p-2">
                不一致
              </th><th class="p-2">
                无声明
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="row in tallyRows" :key="row.key">
              <td class="max-w-48 truncate p-2" :title="row.account">
                {{ row.account }}
              </td><td class="p-2">
                {{ row.model }}
              </td><td class="p-2">
                {{ row.tally.servedMatch ?? 0 }}
              </td><td class="p-2" :class="row.tally.servedMismatch ? 'text-cp-danger' : ''">
                {{ row.tally.servedMismatch ?? 0 }}
              </td><td class="p-2">
                {{ row.tally.servedUnknown ?? 0 }}
              </td>
            </tr>
            <tr v-if="!tallyRows.length">
              <td colspan="5" class="p-6 text-center text-cp-text-secondary">
                暂无业务响应记录
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </BaseCard>
    <details class="text-cp-sm text-cp-text-secondary">
      <summary class="cursor-pointer py-2">
        高级设置与诊断
      </summary>
      <div class="mt-3 flex flex-col gap-5">
        <BaseCard>
          <div v-if="settings" class="flex flex-col gap-4">
            <div class="flex flex-wrap items-end gap-3">
              <div class="w-40">
                <span class="block text-xs text-neutral-500">被动捕获有效期（秒）</span>
                <BaseNumberInput v-model="settings.ttlSeconds" label="模板寿命" :min="30" :max="86400" :step="60" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="w-80">
                <span class="block text-xs text-neutral-500">注入模式</span>
                <BaseSelect v-model="settings.injectMode" :options="injectModeOptions" class="mt-1" @update:model-value="markDirty" />
              </div>
            </div>

            <div class="grid gap-4 md:grid-cols-2">
              <div>
                <span class="block text-xs text-neutral-500">可收录长度</span>
                <div class="mt-1 flex flex-wrap items-center gap-1.5">
                  <span
                    v-for="len in settings.templateLengths" :key="len"
                    class="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-0.5 text-xs text-emerald-600 dark:text-emerald-400"
                  >
                    {{ len }}
                    <button type="button" class="opacity-70 hover:opacity-100" :aria-label="`移除 ${len}`" @click="removeLength('template', len)">×</button>
                  </span>
                  <span v-if="!settings.templateLengths.length" class="text-xs text-neutral-400">空（下限规则）</span>
                  <BaseNumberInput v-model="newTemplateLen" label="新增模板长度" :min="MIN_LEN" :max="4096" class="w-32" />
                  <BaseButton size="sm" @click="addLength('template', newTemplateLen)">
                    加入
                  </BaseButton>
                </div>
              </div>
              <div>
                <span class="block text-xs text-neutral-500">不收录的长度</span>
                <div class="mt-1 flex flex-wrap items-center gap-1.5">
                  <span
                    v-for="len in settings.degradedLengths" :key="len"
                    class="inline-flex items-center gap-1 rounded-full bg-rose-500/10 px-2 py-0.5 text-xs text-rose-600 dark:text-rose-400"
                  >
                    {{ len }}
                    <button type="button" class="opacity-70 hover:opacity-100" :aria-label="`移除 ${len}`" @click="removeLength('degraded', len)">×</button>
                  </span>
                  <span v-if="!settings.degradedLengths.length" class="text-xs text-neutral-400">空</span>
                  <BaseNumberInput v-model="newDegradedLen" label="新增受限长度" :min="MIN_LEN" :max="4096" class="w-32" />
                  <BaseButton size="sm" @click="addLength('degraded', newDegradedLen)">
                    加入
                  </BaseButton>
                </div>
              </div>
            </div>
          </div>
          <p v-else class="m-0 text-sm text-neutral-400">
            设置读取中…
          </p>
        </BaseCard>
        <BaseCard title="连接预热">
          <div v-if="settings" class="flex flex-col gap-4">
            <div class="flex flex-wrap items-center gap-5">
              <SyncedSwitch v-model="settings.warmPool.enabled" label="启用连接预热" show-label @update:model-value="markDirty" />
              <SyncedSwitch v-model="settings.warmPool.businessReuse" label="业务可复用" show-label @update:model-value="markDirty" />
              <SyncedSwitch v-model="settings.warmPool.probe" label="检查探针回答" show-label @update:model-value="markDirty" />
              <SyncedSwitch v-model="settings.warmPool.requireVerified" label="只用已验证连接" show-label @update:model-value="markDirty" />
            </div>
            <p v-if="warmError" class="m-0 text-sm text-cp-danger" role="alert">
              {{ warmError }}
            </p>
            <p v-else-if="settings.warmPool.requireVerified" class="m-0 text-xs text-neutral-500">
              单次最多等待 2 秒，仍无合格连接时返回暂不可用
            </p>
            <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
              <div class="w-40">
                <span class="block text-xs text-neutral-500">每账号、每模型连接数</span>
                <BaseNumberInput v-model="settings.warmPool.connectionsPerAccount" label="每账号每模型连接数" :min="1" :max="16" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="w-40">
                <span class="block text-xs text-neutral-500">探针失败重试次数</span>
                <BaseNumberInput v-model="settings.warmPool.probeRetries" label="探针失败重试" :min="0" :max="16" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="w-40">
                <span class="block text-xs text-neutral-500">复探间隔（秒）</span>
                <BaseNumberInput v-model="settings.warmPool.reprobeSeconds" label="复探间隔" :min="15" :max="1800" :step="15" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="w-40">
                <span class="block text-xs text-neutral-500">连接寿命（秒，&lt;55min）</span>
                <BaseNumberInput v-model="settings.warmPool.maxAgeSeconds" label="连接寿命" :min="60" :max="3300" :step="60" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="w-40">
                <span class="block text-xs text-neutral-500">失败冷却（秒）</span>
                <BaseNumberInput v-model="settings.warmPool.cooldownSeconds" label="失败冷却" :min="30" :max="86400" :step="30" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="w-40">
                <span class="block text-xs text-neutral-500">进程连接总上限</span>
                <BaseNumberInput v-model="settings.warmPool.maxTotalConnections" label="总上限" :min="1" :max="1024" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="w-40">
                <span class="block text-xs text-neutral-500">探针整次上限（秒）</span>
                <BaseNumberInput v-model="settings.warmPool.probeTimeoutSeconds" label="探针上限" :min="30" :max="600" :step="10" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="w-40">
                <span class="block text-xs text-neutral-500">探针 effort</span>
                <BaseSelect v-model="settings.warmPool.probeEffort" :options="effortOptions" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="w-40">
                <span class="block text-xs text-neutral-500">验证通过判据（完整答案）</span>
                <BaseInput v-model="settings.warmPool.probeExpect" placeholder="21" class="mt-1" @update:model-value="markDirty" />
              </div>
              <div class="md:col-span-2">
                <span class="block text-xs text-neutral-500">预热模型（逗号分隔，留空跟随业务）</span>
                <BaseInput v-model="warmModelsText" aria-label="预热模型" placeholder="gpt-6-astra" class="mt-1" />
              </div>
            </div>
            <p class="m-0 text-xs text-neutral-500">
              仅对开启「票与预热」的账号生效
              {{ settings.cloudMint.enabled && hasMintProxy ? '使用专用打票代理筛选，成功后复用同一连接' : '使用账号出口预热连接' }}
              多个模型分别验证，失败按重试次数继续，耗尽后冷却再试
            </p>
          </div>
        </BaseCard>
        <BaseCard title="上游签发长度直方图" description="来自业务流量与探测的响应头，点「设为」把某个长度加进对应表，再保存设置">
          <div v-if="histogram.length" class="flex flex-col gap-1.5">
            <div v-for="row in histogram" :key="row.len" class="flex items-center gap-3 text-sm">
              <span class="w-16 shrink-0 text-right font-mono">{{ row.len }}</span>
              <div class="h-3 flex-1 overflow-hidden rounded bg-neutral-200/60 dark:bg-neutral-800">
                <div
                  class="h-full rounded"
                  :class="row.cls === 'normal' ? 'bg-emerald-500' : row.cls === 'degraded' ? 'bg-rose-500' : 'bg-amber-500'"
                  :style="{ width: `${Math.max(2, row.share * 100)}%` }"
                />
              </div>
              <span class="w-20 shrink-0 text-right text-xs text-neutral-500">{{ row.count }} 次</span>
              <span class="w-14 shrink-0 text-xs" :class="row.cls === 'normal' ? 'text-emerald-500' : row.cls === 'degraded' ? 'text-rose-500' : 'text-amber-500'">
                {{ row.cls === 'normal' ? '符合收录规则' : row.cls === 'degraded' ? '不收录' : '未知' }}
              </span>
              <BaseButton size="sm" variant="soft" @click="addLength('template', row.len)">
                设为模板
              </BaseButton>
              <BaseButton size="sm" variant="soft" @click="addLength('degraded', row.len)">
                设为不收录
              </BaseButton>
            </div>
          </div>
          <p v-else class="m-0 text-sm text-neutral-400">
            还没有观测到任何上游签发的 state
          </p>
        </BaseCard>
        <BaseCard title="最近 48 小时" description="每小时上游签发分布：绿=符合收录规则，红=不收录，黄=未知，灰=未签发">
          <div v-if="hourly.length" class="flex h-28 items-end gap-0.5">
            <div
              v-for="h in hourly" :key="h.hour"
              class="flex flex-1 flex-col-reverse overflow-hidden rounded-sm bg-neutral-200/40 dark:bg-neutral-800/60"
              :title="`${fmtTime(h.hour)}：符合收录规则 ${h.normal} 不收录 ${h.degraded} 未知 ${h.unknown} 未签发 ${h.silent} 注入 ${h.injected}`"
              style="height: 100%"
            >
              <div class="bg-emerald-500" :style="{ height: `${(h.normal / h.max) * 100}%` }" />
              <div class="bg-rose-500" :style="{ height: `${(h.degraded / h.max) * 100}%` }" />
              <div class="bg-amber-500" :style="{ height: `${(h.unknown / h.max) * 100}%` }" />
              <div class="bg-neutral-400" :style="{ height: `${(h.silent / h.max) * 100}%` }" />
            </div>
          </div>
          <p v-else class="m-0 text-sm text-neutral-400">
            暂无数据
          </p>
        </BaseCard>
        <BaseCard title="最近事件" :description="`最近 ${events.length} 条请求决策与上游签发（只记长度，不含票值）`">
          <div class="max-h-96 overflow-auto">
            <table class="w-full text-sm">
              <thead class="text-left text-xs text-neutral-500">
                <tr>
                  <th class="px-2 py-1.5">
                    时间
                  </th>
                  <th class="px-2 py-1.5">
                    账号
                  </th>
                  <th class="px-2 py-1.5">
                    模型
                  </th>
                  <th class="px-2 py-1.5">
                    决策
                  </th>
                  <th class="px-2 py-1.5">
                    注入
                  </th>
                  <th class="px-2 py-1.5">
                    上游签发
                  </th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="e in events" :key="e.id" class="border-t border-neutral-100 dark:border-neutral-800/60">
                  <td class="px-2 py-1 text-xs text-neutral-500">
                    {{ fmtTime(e.at) }}
                  </td>
                  <td class="max-w-56 truncate px-2 py-1 font-mono text-xs" :title="e.account">
                    {{ e.account }}
                  </td>
                  <td class="px-2 py-1 text-xs">
                    {{ e.model }}
                  </td>
                  <td class="px-2 py-1 text-xs">
                    {{ decisionText[e.decision] ?? e.decision }}
                  </td>
                  <td class="px-2 py-1 text-xs">
                    {{ e.injected ? '是' : '否' }}
                  </td>
                  <td class="px-2 py-1 font-mono text-xs" :class="e.len === null ? 'text-neutral-400' : e.class === 'normal' ? 'text-emerald-500' : e.class === 'degraded' ? 'text-rose-500' : 'text-amber-500'">
                    {{ e.len === null ? '未签发' : `${e.len}（${e.class === 'normal' ? '符合收录规则' : e.class === 'degraded' ? '不收录' : '未知'}）` }}
                  </td>
                </tr>
                <tr v-if="!events.length">
                  <td colspan="6" class="px-2 py-6 text-center text-sm text-neutral-400">
                    暂无事件
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </BaseCard>
      </div>
    </details>
  </div>
</template>
