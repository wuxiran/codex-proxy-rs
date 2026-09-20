<script setup lang="ts">
import type { TurnStateHuntRow } from '../composables/useAccountTurnStateHunt'
import type { TurnStateAutoHunt, TurnStateCaptureRule } from '@/api'
import { computed, ref, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseNumberInput from '@/components/base/BaseNumberInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { useProxyCatalog } from '@/composables/useProxyCatalog'
import { useAccountAutoHunt } from '../composables/useAccountAutoHunt'
import { useAccountTurnStateHunt } from '../composables/useAccountTurnStateHunt'

const props = defineProps<{ accountId: string, captureRule: TurnStateCaptureRule | null }>()
const emit = defineEmits<{
  close: []
  /** 命中并完成绑定与钉住；boundChanged 表示账号的代理绑定发生了变化，autoRenew 为要保存的续期参数。 */
  hunted: [boundChanged: boolean, autoRenew: TurnStateAutoHunt | null]
  /** 取消后账号是否被改动只有服务端知道，让上层按实际状态刷新。 */
  cancelled: []
}>()

const PREFERRED_MODEL = 'gpt-6-astra'
const models = computed(() => Object.keys(props.captureRule?.modelLengths ?? {}))
const modelOptions = computed(() => models.value.map(model => ({
  label: `${model}（${props.captureRule!.modelLengths[model]} 字节）`,
  value: model,
})))
const modelId = ref('')
watch(models, (value) => {
  if (!value.includes(modelId.value))
    modelId.value = value.includes(PREFERRED_MODEL) ? PREFERRED_MODEL : value[0] ?? ''
}, { immediate: true })
const attempts = ref(5)
const includeDirect = ref(false)
const autoRenew = ref(true)

const { proxies } = useProxyCatalog()
const usable = computed(() => proxies.value.filter(proxy => proxy.lastTest?.success === true))
const usableProxies = computed(() => usable.value.length)

// 轮换出口每次请求换一个 IP，值得单独打上百次；固定出口同一个 IP 反复打没有意义，
// 所以只有指定了单个代理才放开到 200 次，遍历全部时仍是 20 次。
const ALL_PROXIES = 'all'
const AUTO_RENEW_MAX_ATTEMPTS = 20
const onlyProxyId = ref(ALL_PROXIES)
const proxyOptions = computed(() => [
  { label: '全部已测试通过的代理', value: ALL_PROXIES },
  ...usable.value.map(proxy => ({ label: `只撞：${proxy.name}`, value: proxy.id })),
])
const single = computed(() => onlyProxyId.value !== ALL_PROXIES)
const maxAttempts = computed(() => single.value ? 200 : 20)
watch(maxAttempts, (max) => {
  if (attempts.value > max)
    attempts.value = max
})
watch(usable, (value) => {
  if (single.value && !value.some(proxy => proxy.id === onlyProxyId.value))
    onlyProxyId.value = ALL_PROXIES
})

const hunt = useAccountTurnStateHunt()
const { status, rows, expectedLength, requests, message } = hunt
const sweepBusy = computed(() => status.value === 'running' || status.value === 'finalizing')

// 模式：sweep = 遍历已存代理；auto = 从轮换代理模板即时生成多国临时出口反复撞，命中切静态。
const mode = ref<'sweep' | 'auto'>('sweep')
const modeOptions = [
  { label: '遍历已存代理', value: 'sweep' },
  { label: '自动撞（多国轮换）', value: 'auto' },
]

// 轮换代理模板：默认名字里带 dongtai 的；否则第一个测试通过的。
const templateProxyId = ref('')
const templateOptions = computed(() =>
  usable.value.map(proxy => ({ label: `${proxy.name}（${proxy.endpoint}）`, value: proxy.id })))
watch(usable, (value) => {
  if (!value.some(proxy => proxy.id === templateProxyId.value)) {
    templateProxyId.value = value.find(proxy => /dongtai|rotate|轮换/i.test(proxy.name))?.id
      ?? value[0]?.id ?? ''
  }
}, { immediate: true })

// 静态出口池：默认选上非轮换、非 dt- 批量的（= 真正的固定家宽）。
const HUNT_COUNTRIES = ['US', 'JP', 'DE', 'PH'] as const
const selectedCountries = ref<string[]>([...HUNT_COUNTRIES])
const staticIds = ref<string[]>([])
const staticDefaulted = ref(false)
watch(usable, (value) => {
  if (!staticDefaulted.value && value.length) {
    staticIds.value = value
      .filter(proxy => !/^dt-|dongtai|rotate|轮换/i.test(proxy.name))
      .map(proxy => proxy.id)
    staticDefaulted.value = true
  }
  staticIds.value = staticIds.value.filter(id => value.some(proxy => proxy.id === id))
}, { immediate: true })
function toggleCountry(code: string) {
  selectedCountries.value = selectedCountries.value.includes(code)
    ? selectedCountries.value.filter(item => item !== code)
    : [...selectedCountries.value, code]
}
function toggleStatic(id: string) {
  staticIds.value = staticIds.value.includes(id)
    ? staticIds.value.filter(item => item !== id)
    : [...staticIds.value, id]
}
const maxIps = ref(300)

const auto = useAccountAutoHunt()
const autoBusy = computed(() => auto.status.value === 'running' || auto.status.value === 'finalizing')

const busy = computed(() => sweepBusy.value || autoBusy.value)
const canStart = computed(() => !busy.value
  && Boolean(modelId.value)
  && attempts.value >= 1 && attempts.value <= maxAttempts.value
  && (usableProxies.value > 0 || (includeDirect.value && !single.value)))
const canStartAuto = computed(() => !busy.value
  && Boolean(modelId.value)
  && Boolean(templateProxyId.value)
  && selectedCountries.value.length > 0
  && staticIds.value.length > 0
  && maxIps.value >= 1 && maxIps.value <= 2000)

function start() {
  hunt.start({
    accountId: props.accountId,
    modelId: modelId.value,
    attempts: attempts.value,
    includeDirect: includeDirect.value && !single.value,
    proxyId: single.value ? onlyProxyId.value : null,
  })
}

function startAuto() {
  auto.start({
    accountId: props.accountId,
    modelId: modelId.value,
    templateProxyId: templateProxyId.value,
    countries: selectedCountries.value,
    staticProxyIds: staticIds.value,
    maxIps: maxIps.value,
  })
}

watch(status, (value) => {
  if (value === 'cancelled')
    emit('cancelled')
  if (value === 'success') {
    emit('hunted', hunt.boundChanged.value, autoRenew.value
      // 续期参数的上限仍是 20 次：命中后账号已绑到该出口，续期会最先打它。
      ? { modelId: modelId.value, attempts: Math.min(attempts.value, AUTO_RENEW_MAX_ATTEMPTS), includeDirect: includeDirect.value }
      : null)
  }
})

// 自动撞命中后账号已切到静态出口：让上层刷新账号，但不写自动续期（续期走遍历，与撞解耦）。
watch(auto.status, (value) => {
  if (value === 'cancelled')
    emit('cancelled')
  if (value === 'success')
    emit('hunted', auto.boundChanged.value, null)
})

const STATE_TEXT: Record<TurnStateHuntRow['state'], string> = {
  pending: '等待',
  running: '探测中',
  hit: '命中',
  miss: '未命中',
  skipped: '已跳过',
  aborted: '已中止',
  notRun: '未执行',
}

function stateText(row: TurnStateHuntRow) {
  if (row.state !== 'skipped')
    return STATE_TEXT[row.state]
  if (row.skipped === 'unavailable')
    return '已跳过：代理不可用'
  return row.skipped === 'capacity' ? '已跳过：上游暂无容量' : '已跳过：出口不通'
}

function attemptText(attempt: TurnStateHuntRow['attempts'][number]) {
  if (attempt.error)
    return attempt.error.upstreamStatus ? `失败 ${attempt.error.upstreamStatus}` : '失败'
  return attempt.length === null ? '无 state' : `${attempt.length}`
}
</script>

<template>
  <section class="min-w-0 rounded-cp bg-cp-bg-container p-3" aria-label="遍历代理找 state">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <h4 class="m-0 text-cp-sm font-heavy text-cp-text">
        遍历代理找 state
      </h4>
      <BaseButton variant="soft" size="sm" :disabled="busy" @click="emit('close')">
        收起
      </BaseButton>
    </div>
    <p class="mb-0 mt-2 text-cp-xs text-cp-text-secondary">
      依次经每个已测试通过的代理向上游发真实请求，直到返回符合长度规则的 state；命中后把账号绑定到该代理，并把这个 state 钉给该账号此模型的全部客户端（替换已有的固定）。每次尝试都会消耗少量额度，未命中不改动账号。勾选自动续期后，服务端会在到期前 5 分钟用同样的参数重新遍历：先试当前绑定的代理，续不上就继续打其它代理；整轮都没续上则 5 分钟后再来。
    </p>
    <p v-if="!models.length" role="alert" class="mb-0 mt-2 text-cp-sm text-cp-error">
      该账号的套餐没有按模型的长度规则，无法判断哪个 state 正确。
    </p>
    <div v-else class="mt-3 grid gap-1 text-cp-xs text-cp-text-secondary">
      <span>方式</span>
      <BaseSelect v-model="mode" class="min-w-56" size="sm" aria-label="方式" :options="modeOptions" :disabled="busy" />
    </div>
    <p v-if="models.length && mode === 'auto'" class="mb-0 mt-2 text-cp-xs text-cp-text-secondary">
      从轮换代理模板即时生成美/日/德/菲随机出口，逐个 IP 打 1 次（同一 IP 复打会被上游抹掉 state），命中即把账号改绑到你选的静态出口并按静态出口钉住 state（state 已确认可跨 IP 移植）。每个 IP 都是一次真实请求、消耗额度，到「最多 IP 数」即止。
    </p>
    <div v-if="models.length && mode === 'sweep'" class="mt-3 flex flex-wrap items-end gap-3">
      <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
        <span>模型</span>
        <BaseSelect v-model="modelId" class="min-w-56" size="sm" aria-label="模型" :options="modelOptions" :disabled="busy" />
      </div>
      <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
        <span>范围</span>
        <BaseSelect v-model="onlyProxyId" class="min-w-56" size="sm" aria-label="遍历范围" :options="proxyOptions" :disabled="busy" />
      </div>
      <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
        <span>{{ single ? '最多尝试（轮换出口可到 200）' : '每个代理最多尝试' }}</span>
        <BaseNumberInput v-model="attempts" label="每个代理最多尝试次数" :min="1" :max="maxAttempts" unit="次" :disabled="busy" />
      </div>
      <div class="pb-2 text-cp-sm text-cp-text">
        <BaseCheckbox v-model="includeDirect" label="同时尝试直连" show-label :disabled="busy || single" />
      </div>
      <div class="pb-2 text-cp-sm text-cp-text">
        <BaseCheckbox v-model="autoRenew" label="到期前自动续期" show-label :disabled="busy" />
      </div>
      <div class="flex gap-2 pb-1">
        <BaseButton variant="primary" size="sm" :disabled="!canStart" @click="start">
          {{ status === 'idle' ? '开始遍历' : '重新遍历' }}
        </BaseButton>
        <BaseButton v-if="busy" variant="secondary" size="sm" :disabled="status === 'finalizing'" @click="hunt.cancel">
          取消
        </BaseButton>
      </div>
    </div>
    <div v-if="models.length && mode === 'auto'" class="mt-3 grid gap-3">
      <div class="flex flex-wrap items-end gap-3">
        <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
          <span>模型</span>
          <BaseSelect v-model="modelId" class="min-w-56" size="sm" aria-label="模型" :options="modelOptions" :disabled="busy" />
        </div>
        <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
          <span>轮换代理模板</span>
          <BaseSelect v-model="templateProxyId" class="min-w-64" size="sm" aria-label="轮换代理模板" :options="templateOptions" :disabled="busy" />
        </div>
        <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
          <span>最多 IP 数（额度闸）</span>
          <BaseNumberInput v-model="maxIps" label="最多生成多少个临时 IP" :min="1" :max="2000" unit="个" :disabled="busy" />
        </div>
      </div>
      <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
        <span>出口国家（随机轮换）</span>
        <div class="flex flex-wrap gap-3">
          <BaseCheckbox
            v-for="code in HUNT_COUNTRIES"
            :key="code"
            :model-value="selectedCountries.includes(code)"
            :label="code"
            show-label
            :disabled="busy"
            @update:model-value="toggleCountry(code)"
          />
        </div>
      </div>
      <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
        <span>命中后切到的静态出口（选账号数最少的一个）</span>
        <div class="flex flex-wrap gap-x-3 gap-y-1.5">
          <BaseCheckbox
            v-for="proxy in usable"
            :key="proxy.id"
            :model-value="staticIds.includes(proxy.id)"
            :label="proxy.name"
            show-label
            :disabled="busy"
            @update:model-value="toggleStatic(proxy.id)"
          />
        </div>
      </div>
      <div class="flex gap-2">
        <BaseButton variant="primary" size="sm" :disabled="!canStartAuto" @click="startAuto">
          {{ auto.status.value === 'idle' ? '开始自动撞' : '重新自动撞' }}
        </BaseButton>
        <BaseButton v-if="autoBusy" variant="secondary" size="sm" :disabled="auto.status.value === 'finalizing'" @click="auto.cancel">
          停止
        </BaseButton>
      </div>
      <p v-if="!usable.length" class="mb-0 text-cp-xs text-cp-error">
        没有测试通过的代理：请先到代理页添加轮换代理模板与静态出口并测试通过。
      </p>
    </div>

    <div v-if="mode === 'auto' && auto.status.value !== 'idle'" class="mt-3 grid gap-2 rounded-cp bg-cp-fill-quaternary px-3 py-2.5 text-cp-sm">
      <div class="flex flex-wrap gap-x-4 gap-y-1">
        <span>已试 IP <b>{{ auto.ipsTried.value }}</b><span class="text-cp-text-secondary"> / {{ auto.totalPlanned.value || maxIps }}</span></span>
        <span>已发请求 <b>{{ auto.requests.value }}</b></span>
        <span>目标 <b>{{ auto.expectedLength.value ?? '…' }}</b> 字节</span>
        <span v-if="auto.lastLength.value !== null" class="text-cp-text-secondary">最近一次 {{ auto.lastLength.value }} 字节</span>
        <span v-if="auto.currentCountry.value" class="text-cp-text-secondary">当前 {{ auto.currentCountry.value }}</span>
      </div>
      <div v-if="auto.countryRows.value.length" class="flex flex-wrap gap-1.5 text-cp-xs text-cp-text-secondary">
        <span v-for="[country, count] in auto.countryRows.value" :key="country" class="rounded-cp-sm bg-cp-fill-tertiary px-1.5 py-0.5">
          {{ country }} × {{ count }}
        </span>
      </div>
      <p
        class="mb-0 text-cp-sm"
        :role="auto.status.value === 'error' ? 'alert' : 'status'"
        :class="auto.status.value === 'error' ? 'text-cp-error' : auto.status.value === 'success' ? 'text-cp-success' : 'text-cp-text-secondary'"
      >
        <template v-if="auto.status.value === 'running'">
          自动撞进行中…（页面关掉也会在后台继续）
        </template>
        <template v-else-if="auto.status.value === 'finalizing'">
          已命中，正在切静态出口并钉住 state…
        </template>
        <template v-else>
          {{ auto.message.value }}
        </template>
      </p>
    </div>

    <p v-if="models.length && mode === 'sweep' && status === 'idle'" class="mb-0 mt-2 text-cp-xs text-cp-text-secondary">
      可用代理 {{ usableProxies }} 个<template v-if="!usableProxies">
        ；请先到代理页把代理测试通过，或勾选「同时尝试直连」
      </template>
    </p>

    <div v-if="mode === 'sweep' && rows.length" class="mt-3 grid gap-1.5">
      <div
        v-for="row in rows"
        :key="row.key"
        class="grid gap-1 rounded-cp px-2.5 py-2 text-cp-sm"
        :class="row.state === 'hit' ? 'bg-cp-success-container' : 'bg-cp-fill-quaternary'"
      >
        <div class="flex flex-wrap items-center justify-between gap-2">
          <span class="min-w-0 truncate text-cp-text">
            {{ row.name }}
            <span v-if="row.endpoint" class="text-cp-xs text-cp-text-secondary">{{ row.endpoint }}</span>
          </span>
          <span class="shrink-0 text-cp-xs" :class="row.state === 'hit' ? 'text-cp-success' : 'text-cp-text-secondary'">
            {{ stateText(row) }}
          </span>
        </div>
        <div v-if="row.attempts.length" class="flex flex-wrap gap-1.5 text-cp-xs">
          <span
            v-for="attempt in row.attempts"
            :key="attempt.index"
            class="rounded-cp-sm px-1.5 py-0.5"
            :class="attempt.matched ? 'bg-cp-success text-white' : attempt.error ? 'text-cp-error' : 'text-cp-text-secondary'"
            :title="attempt.error?.message"
          >
            #{{ attempt.index }} {{ attemptText(attempt) }}
          </span>
        </div>
      </div>
    </div>

    <p
      v-if="mode === 'sweep' && status !== 'idle'"
      class="mb-0 mt-3 text-cp-sm"
      :role="status === 'error' ? 'alert' : 'status'"
      :class="status === 'error' ? 'text-cp-error' : status === 'success' ? 'text-cp-success' : 'text-cp-text-secondary'"
    >
      <template v-if="status === 'running'">
        遍历中，目标 {{ expectedLength ?? '…' }} 字节，已尝试 {{ requests }} 次
      </template>
      <template v-else-if="status === 'finalizing'">
        已命中，正在绑定代理并钉住 state…
      </template>
      <template v-else>
        {{ message }}<template v-if="status !== 'error' && requests">
          （共 {{ requests }} 次）
        </template>
      </template>
    </p>
  </section>
</template>
