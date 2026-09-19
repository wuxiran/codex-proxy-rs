<script setup lang="ts">
import type { TurnStateHuntRow } from '../composables/useAccountTurnStateHunt'
import type { TurnStateAutoHunt, TurnStateCaptureRule } from '@/api'
import { computed, ref, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseNumberInput from '@/components/base/BaseNumberInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { useProxyCatalog } from '@/composables/useProxyCatalog'
import { useAccountTurnStateHunt } from '../composables/useAccountTurnStateHunt'

const props = defineProps<{ accountId: string, captureRule: TurnStateCaptureRule | null }>()
const emit = defineEmits<{
  close: []
  /** 命中并完成绑定与钉住；boundChanged 表示账号的代理绑定发生了变化，autoRenew 为要保存的续期参数。 */
  hunted: [boundChanged: boolean, autoRenew: TurnStateAutoHunt | null]
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
const usableProxies = computed(() => proxies.value.filter(proxy => proxy.lastTest?.success === true).length)

const hunt = useAccountTurnStateHunt()
const { status, rows, expectedLength, requests, message } = hunt
const busy = computed(() => status.value === 'running' || status.value === 'finalizing')
const canStart = computed(() => !busy.value
  && Boolean(modelId.value)
  && attempts.value >= 1 && attempts.value <= 20
  && (usableProxies.value > 0 || includeDirect.value))

function start() {
  hunt.start({
    accountId: props.accountId,
    modelId: modelId.value,
    attempts: attempts.value,
    includeDirect: includeDirect.value,
  })
}

watch(status, (value) => {
  if (value === 'success') {
    emit('hunted', hunt.boundChanged.value, autoRenew.value
      ? { modelId: modelId.value, attempts: attempts.value, includeDirect: includeDirect.value }
      : null)
  }
})

const STATE_TEXT: Record<TurnStateHuntRow['state'], string> = {
  pending: '等待',
  running: '探测中',
  hit: '命中',
  miss: '未命中',
  skipped: '已跳过',
}

function stateText(row: TurnStateHuntRow) {
  if (row.state !== 'skipped')
    return STATE_TEXT[row.state]
  return row.skipped === 'unavailable' ? '已跳过：代理不可用' : '已跳过：出口不通'
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
    <div v-else class="mt-3 flex flex-wrap items-end gap-3">
      <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
        <span>模型</span>
        <BaseSelect v-model="modelId" class="min-w-56" size="sm" aria-label="模型" :options="modelOptions" :disabled="busy" />
      </div>
      <div class="grid gap-1 text-cp-xs text-cp-text-secondary">
        <span>每个代理最多尝试</span>
        <BaseNumberInput v-model="attempts" label="每个代理最多尝试次数" :min="1" :max="20" unit="次" :disabled="busy" />
      </div>
      <div class="pb-2 text-cp-sm text-cp-text">
        <BaseCheckbox v-model="includeDirect" label="同时尝试直连" show-label :disabled="busy" />
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
    <p v-if="models.length && status === 'idle'" class="mb-0 mt-2 text-cp-xs text-cp-text-secondary">
      可用代理 {{ usableProxies }} 个<template v-if="!usableProxies">
        ；请先到代理页把代理测试通过，或勾选「同时尝试直连」
      </template>
    </p>

    <div v-if="rows.length" class="mt-3 grid gap-1.5">
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
      v-if="status !== 'idle'"
      class="mb-0 mt-3 text-cp-sm"
      :role="status === 'error' ? 'alert' : 'status'"
      :class="status === 'error' ? 'text-cp-error' : status === 'success' ? 'text-cp-success' : 'text-cp-text-secondary'"
    >
      <template v-if="status === 'running'">
        遍历中，目标 {{ expectedLength ?? '…' }} 字节，已发出 {{ requests }} 次请求
      </template>
      <template v-else-if="status === 'finalizing'">
        已命中，正在绑定代理并钉住 state…
      </template>
      <template v-else>
        {{ message }}<template v-if="status !== 'error' && requests">
          （共 {{ requests }} 次请求）
        </template>
      </template>
    </p>
  </section>
</template>
