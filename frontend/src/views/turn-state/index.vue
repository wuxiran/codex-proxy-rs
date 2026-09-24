<script setup lang="ts">
// state 观测：turn-state 模板桶（账号 × 模型）的运行设置、长度直方图、桶列表与服务态观测。
// 「注入中 · 盲区」不等于正常——桶里有模板时每个请求都被注入，上游不再签发新 state，观测不到数据；
// 注入后上游仍签发受限档才是唯一要处理的信号（模板救不了这个桶）。
import type {
  TurnStateBucket,
  TurnStateBucketTally,
  TurnStateInjectMode,
  TurnStateLengthClass,
  TurnStateObservations,
  TurnStateSettings,
} from '@/api'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  clearTurnStateBucket,
  getTurnStateBuckets,
  getTurnStateObservations,
  getTurnStateSettings,
  updateTurnStateSettings,
} from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseNumberInput from '@/components/base/BaseNumberInput.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { toast } from '@/components/base/BaseToast'

const MIN_LEN = 200
const POLL_MS = 30_000

const settings = ref<TurnStateSettings | null>(null)
const settingsDirty = ref(false)
const saving = ref(false)
const buckets = ref<TurnStateBucket[]>([])
const bucketsNow = ref(0)
const observations = ref<TurnStateObservations | null>(null)
const loading = ref(false)
const newTemplateLen = ref(MIN_LEN)
const newDegradedLen = ref(MIN_LEN)

const mintModeOptions = [
  { label: 'native：cpr 经账号代理直打上游', value: 'native' },
  { label: 'relay：交给 cloud-mint relay', value: 'relay' },
]
const transportOptions = [
  { label: 'SSE', value: 'sse' },
  { label: 'WebSocket', value: 'websocket' },
]
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

const injectModeOptions: Array<{ label: string, value: TurnStateInjectMode }> = [
  { label: 'always：有模板就注入（补上/换掉）', value: 'always' },
  { label: 'replace-only：只替换受限档 state', value: 'replace-only' },
]

async function loadSettings() {
  try {
    settings.value = await getTurnStateSettings({ silent: true })
    settingsDirty.value = false
  }
  catch {
    toast.error('turn-state 设置读取失败')
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
  if (!settings.value)
    return
  saving.value = true
  try {
    settings.value = await updateTurnStateSettings(settings.value)
    settingsDirty.value = false
    toast.success('已保存，热生效（多实例 1 秒内同步）')
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
  label: string
  tone: 'ok' | 'bad' | 'warn' | 'muted'
}

const tallyRows = computed<TallyRow[]>(() => {
  const raw = observations.value?.buckets ?? {}
  return Object.entries(raw).map(([key, tally]) => {
    const slash = key.indexOf('/')
    const account = slash >= 0 ? key.slice(0, slash) : key
    const model = slash >= 0 ? key.slice(slash + 1) : ''
    return { key, account, model, tally, ...statusOf(tally) }
  }).sort((a, b) => b.tally.lastSeenAt - a.tally.lastSeenAt)
})

function statusOf(t: TurnStateBucketTally): { label: string, tone: TallyRow['tone'] } {
  const seen = t.normal + t.degraded + t.unknown + t.silent
  if (!seen)
    return { label: '— 无观测', tone: 'muted' }
  if (t.lastIssuedLen !== null) {
    const cls = classify(t.lastIssuedLen)
    // 上游最近一次签发的长度；是否注入不影响新鲜度判定。
    if (cls === 'degraded') {
      return t.injectedDegraded > 0
        ? { label: `▲ 受限 · 模板失效 (${t.lastIssuedLen})`, tone: 'bad' }
        : { label: `● 受限 (${t.lastIssuedLen})`, tone: 'bad' }
    }
    if (cls === 'normal')
      return { label: `● 正常 (${t.lastIssuedLen})`, tone: 'ok' }
    return { label: `? 未知格式 (${t.lastIssuedLen})`, tone: 'warn' }
  }
  if (t.injectedSilent > 0)
    return { label: '◌ 注入中 · 盲区', tone: 'warn' }
  return { label: '· 无流量', tone: 'muted' }
}

const toneClass: Record<TallyRow['tone'], string> = {
  ok: 'text-emerald-500',
  bad: 'text-rose-500',
  warn: 'text-amber-500',
  muted: 'text-neutral-400',
}

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
  const minutes = Math.floor(delta / 60)
  return minutes >= 60 ? `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分` : `${minutes} 分`
}

const sourceText: Record<TurnStateBucket['source'], string> = {
  hunt: '遍历命中',
  passive: '被动捕获',
  renewal: '自动续期',
  mint: '云端打票',
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
  <div class="flex w-full flex-col gap-5 px-4 py-6">
    <BasePageHeader
      title="state 观测"
      description="Codex turn-state 模板按「账号 × 模型」分桶落盘，蓝绿实例共享。这里看上游签发了哪一档长度、模板有没有被接受，并配置长度档位与注入策略。"
    />

    <BaseCard title="运行设置" description="整体替换，保存即热生效。长度表为空时退回「≥200 字节可见 ASCII 即可入库」的下限规则。">
      <div v-if="settings" class="flex flex-col gap-4">
        <div class="flex flex-wrap items-end gap-3">
          <div class="w-40">
            <span class="block text-xs text-neutral-500">模板寿命（秒）</span>
            <BaseNumberInput v-model="settings.ttlSeconds" label="模板寿命" :min="600" :max="86400" :step="60" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div class="w-80">
            <span class="block text-xs text-neutral-500">注入模式</span>
            <BaseSelect v-model="settings.injectMode" :options="injectModeOptions" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div class="flex items-center gap-4 pb-2">
            <BaseCheckbox v-model="settings.dryRun" label="模拟运行（只记决策不改请求）" show-label @update:model-value="markDirty" />
            <BaseCheckbox v-model="settings.logDecisions" label="打印决策日志" show-label @update:model-value="markDirty" />
          </div>
          <BaseButton variant="primary" :loading="saving" :disabled="!settingsDirty || saving" @click="saveSettings">
            保存设置
          </BaseButton>
        </div>

        <div class="grid gap-4 md:grid-cols-2">
          <div>
            <span class="block text-xs text-neutral-500">模板长度（正常档，可入库）</span>
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
            <span class="block text-xs text-neutral-500">受限长度（降级档，永不入库；replace-only 只替换这些）</span>
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

    <BaseCard title="云端打票" description="账号缺票时向 relay（deploy/cloud-mint）铸票：验收票长与目标网关，把路由 cookie 对写进凭据、票钉成账号级模板；票只有约 240 秒，后台对最近有流量的账号到期前自动续打。">
      <div v-if="settings" class="flex flex-col gap-4">
        <div class="flex flex-wrap items-center gap-5">
          <BaseSwitch v-model="settings.cloudMint.enabled" label="启用云端打票" show-label @update:model-value="markDirty" />
          <BaseSwitch v-model="settings.cloudMint.observeOnly" label="仅观测，不注入" show-label @update:model-value="markDirty" />
          <BaseButton variant="primary" :loading="saving" :disabled="!settingsDirty || saving" @click="saveSettings">
            保存设置
          </BaseButton>
        </div>
        <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          <div>
            <span class="block text-xs text-neutral-500">打票方式</span>
            <BaseSelect v-model="settings.cloudMint.mode" :options="mintModeOptions" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div class="w-40">
            <span class="block text-xs text-neutral-500">每模型最多尝试</span>
            <BaseNumberInput v-model="settings.cloudMint.maxAttempts" label="每模型最多尝试" :min="1" :max="64" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div v-if="settings.cloudMint.mode === 'relay'">
            <span class="block text-xs text-neutral-500">relay 地址</span>
            <BaseInput v-model="settings.cloudMint.relayUrl" placeholder="http://127.0.0.1:9000" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div v-if="settings.cloudMint.mode === 'relay'">
            <span class="block text-xs text-neutral-500">X-Relay-Key（已保存则显示 &lt;set&gt;，留着不改即沿用）</span>
            <BaseInput v-model="relayKeyInput" type="password" placeholder="relay 的 RELAY_KEY" class="mt-1" />
          </div>
          <div v-if="settings.cloudMint.mode === 'relay'">
            <span class="block text-xs text-neutral-500">前置代理（插件 → relay；空 = 直连）</span>
            <BaseInput v-model="settings.cloudMint.proxyUrl" placeholder="socks5h://user:pw@host:1080" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div class="w-48">
            <span class="block text-xs text-neutral-500">目标网关（空 = 任意）</span>
            <BaseInput v-model="settings.cloudMint.gateway" placeholder="unified-95" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div class="w-40">
            <span class="block text-xs text-neutral-500">预期票长（0 = 不查）</span>
            <BaseNumberInput v-model="settings.cloudMint.ticketLen" label="预期票长" :min="0" :max="4096" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div class="w-40">
            <span class="block text-xs text-neutral-500">票有效期（秒）</span>
            <BaseNumberInput v-model="settings.cloudMint.ticketTtlSeconds" label="票有效期" :min="30" :max="86400" :step="30" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div v-if="settings.cloudMint.mode === 'relay'" class="w-40">
            <span class="block text-xs text-neutral-500">打票协议</span>
            <BaseSelect v-model="settings.cloudMint.transport" :options="transportOptions" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div class="w-40">
            <span class="block text-xs text-neutral-500">失败冷却（秒）</span>
            <BaseNumberInput v-model="settings.cloudMint.cooldownSeconds" label="失败冷却" :min="5" :max="3600" :step="5" class="mt-1" @update:model-value="markDirty" />
          </div>
          <div class="md:col-span-2">
            <span class="block text-xs text-neutral-500">打票模型（逗号分隔；空 = 按业务请求的模型）</span>
            <BaseInput v-model="mintModelsText" placeholder="gpt-6-astra, gpt-6-sol" class="mt-1" />
          </div>
        </div>
        <p class="m-0 text-xs text-neutral-500">
          native 模式走账号自己绑定的代理出网（出口 = 该代理 IP），relay 模式出口是 relay 所在机器。780 只是预期格式长度，网关名与模型声明都不能证明模型实际能力；是否满血仍以测智台人工判定为准。缺票的账号先裸发一次业务请求触发预热，下一次请求才带票。
        </p>
      </div>
    </BaseCard>

    <BaseCard title="上游签发长度直方图" description="来自业务流量与探测的响应头；点「设为」把某个长度加进对应表，再保存设置。">
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
            {{ row.cls === 'normal' ? '正常' : row.cls === 'degraded' ? '受限' : '未知' }}
          </span>
          <BaseButton size="sm" variant="soft" @click="addLength('template', row.len)">
            设为模板
          </BaseButton>
          <BaseButton size="sm" variant="soft" @click="addLength('degraded', row.len)">
            设为受限
          </BaseButton>
        </div>
      </div>
      <p v-else class="m-0 text-sm text-neutral-400">
        还没有观测到任何上游签发的 state。
      </p>
    </BaseCard>

    <BaseCard title="模板桶" :description="`账号级模板落盘共享；客户端级为本实例内存被动捕获。共 ${buckets.length} 条${loading ? '，刷新中…' : ''}`">
      <div class="overflow-auto">
        <table class="w-full text-sm">
          <thead class="text-left text-xs text-neutral-500">
            <tr>
              <th class="px-2 py-1.5">
                账号
              </th>
              <th class="px-2 py-1.5">
                模型
              </th>
              <th class="px-2 py-1.5">
                范围
              </th>
              <th class="px-2 py-1.5">
                长度
              </th>
              <th class="px-2 py-1.5">
                签发
              </th>
              <th class="px-2 py-1.5">
                剩余
              </th>
              <th class="px-2 py-1.5">
                来源
              </th>
              <th class="px-2 py-1.5">
                网关
              </th>
              <th class="px-2 py-1.5">
                出口
              </th>
              <th class="px-2 py-1.5">
                命中
              </th>
              <th class="px-2 py-1.5" />
            </tr>
          </thead>
          <tbody>
            <tr v-for="b in buckets" :key="`${b.account}/${b.model}/${b.scope}/${b.hits}`" class="border-t border-neutral-100 dark:border-neutral-800/60">
              <td class="max-w-56 truncate px-2 py-1.5 font-mono text-xs" :title="b.account">
                {{ b.account }}
              </td>
              <td class="px-2 py-1.5">
                {{ b.model }}
              </td>
              <td class="px-2 py-1.5 text-xs">
                {{ b.scope === 'account' ? '账号级' : '客户端级' }}
              </td>
              <td class="px-2 py-1.5 font-mono" :class="classify(b.len) === 'normal' ? 'text-emerald-500' : classify(b.len) === 'degraded' ? 'text-rose-500' : 'text-amber-500'">
                {{ b.len }}
              </td>
              <td class="px-2 py-1.5 text-xs" :title="b.issuedAtSource === 'fernet' ? '从票内嵌 Fernet 时间戳读出' : '票不是 Fernet 形状，按捕获时刻算'">
                {{ fmtTime(b.issuedAt) }}
                <span class="ml-1 text-neutral-400">{{ b.issuedAtSource === 'fernet' ? '票内' : '捕获' }}</span>
              </td>
              <td class="px-2 py-1.5 text-xs">
                {{ fmtRemaining(b.expiresAt) }}
              </td>
              <td class="px-2 py-1.5 text-xs">
                {{ sourceText[b.source] }}
              </td>
              <td class="px-2 py-1.5 font-mono text-xs">
                {{ b.gateway ?? '—' }}
              </td>
              <td class="px-2 py-1.5 font-mono text-xs text-neutral-400">
                {{ b.egress ?? '—' }}
              </td>
              <td class="px-2 py-1.5">
                {{ b.hits }}
              </td>
              <td class="px-2 py-1.5 text-right">
                <BaseButton v-if="b.scope === 'account'" size="sm" variant="soft" @click="clearBucket(b)">
                  清除
                </BaseButton>
              </td>
            </tr>
            <tr v-if="!buckets.length">
              <td colspan="11" class="px-2 py-6 text-center text-sm text-neutral-400">
                没有有效模板。到账号页「遍历代理找 state」/「云端打票」或等待业务流量被动捕获。
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </BaseCard>

    <BaseCard title="按桶观测" description="正常/受限/未知按上游签发长度计；「注入」列区分本请求是否发了模板——注入后上游沉默是盲区，不是正常。">
      <div class="overflow-auto">
        <table class="w-full text-sm">
          <thead class="text-left text-xs text-neutral-500">
            <tr>
              <th class="px-2 py-1.5">
                账号
              </th>
              <th class="px-2 py-1.5">
                模型
              </th>
              <th class="px-2 py-1.5">
                状态
              </th>
              <th class="px-2 py-1.5">
                正常
              </th>
              <th class="px-2 py-1.5">
                受限
              </th>
              <th class="px-2 py-1.5">
                未知
              </th>
              <th class="px-2 py-1.5">
                沉默
              </th>
              <th class="px-2 py-1.5" title="注入总数 / 注入后沉默（盲区） / 注入后仍受限（模板失效）">
                注入 / 盲区 / 失效
              </th>
              <th class="px-2 py-1.5">
                替换
              </th>
              <th class="px-2 py-1.5">
                最近
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="row in tallyRows" :key="row.key" class="border-t border-neutral-100 dark:border-neutral-800/60">
              <td class="max-w-56 truncate px-2 py-1.5 font-mono text-xs" :title="row.account">
                {{ row.account }}
              </td>
              <td class="px-2 py-1.5">
                {{ row.model }}
              </td>
              <td class="px-2 py-1.5 text-xs" :class="toneClass[row.tone]">
                {{ row.label }}
              </td>
              <td class="px-2 py-1.5">
                {{ row.tally.normal }}
              </td>
              <td class="px-2 py-1.5">
                {{ row.tally.degraded }}
              </td>
              <td class="px-2 py-1.5">
                {{ row.tally.unknown }}
              </td>
              <td class="px-2 py-1.5">
                {{ row.tally.silent }}
              </td>
              <td class="px-2 py-1.5 font-mono text-xs">
                {{ row.tally.injectedTotal }} / {{ row.tally.injectedSilent }} / <span :class="row.tally.injectedDegraded ? 'text-rose-500' : ''">{{ row.tally.injectedDegraded }}</span>
              </td>
              <td class="px-2 py-1.5">
                {{ row.tally.substituted }}
              </td>
              <td class="px-2 py-1.5 text-xs text-neutral-500">
                {{ fmtTime(row.tally.lastSeenAt) }}
              </td>
            </tr>
            <tr v-if="!tallyRows.length">
              <td colspan="10" class="px-2 py-6 text-center text-sm text-neutral-400">
                暂无观测。
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </BaseCard>

    <BaseCard title="最近 48 小时" description="每小时上游签发分布：绿=正常，红=受限，黄=未知，灰=沉默。">
      <div v-if="hourly.length" class="flex h-28 items-end gap-0.5">
        <div
          v-for="h in hourly" :key="h.hour"
          class="flex flex-1 flex-col-reverse overflow-hidden rounded-sm bg-neutral-200/40 dark:bg-neutral-800/60"
          :title="`${fmtTime(h.hour)}：正常 ${h.normal} 受限 ${h.degraded} 未知 ${h.unknown} 沉默 ${h.silent} 注入 ${h.injected}`"
          style="height: 100%"
        >
          <div class="bg-emerald-500" :style="{ height: `${(h.normal / h.max) * 100}%` }" />
          <div class="bg-rose-500" :style="{ height: `${(h.degraded / h.max) * 100}%` }" />
          <div class="bg-amber-500" :style="{ height: `${(h.unknown / h.max) * 100}%` }" />
          <div class="bg-neutral-400" :style="{ height: `${(h.silent / h.max) * 100}%` }" />
        </div>
      </div>
      <p v-else class="m-0 text-sm text-neutral-400">
        暂无数据。
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
                {{ e.len === null ? '沉默' : `${e.len}（${e.class === 'normal' ? '正常' : e.class === 'degraded' ? '受限' : '未知'}）` }}
              </td>
            </tr>
            <tr v-if="!events.length">
              <td colspan="6" class="px-2 py-6 text-center text-sm text-neutral-400">
                暂无事件。
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </BaseCard>
  </div>
</template>
