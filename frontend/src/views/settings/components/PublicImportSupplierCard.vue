<script setup lang="ts">
import type { AccountGroup, PublicImportConfig } from '@/api'
import { Copy, RefreshCw, Save, Trash2 } from '@lucide/vue'
import { computed, ref, shallowRef, watch } from 'vue'

import { deletePublicImportConfig, rotatePublicImportToken, updatePublicImportConfig } from '@/api'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useCopyText } from '@/composables/useCopyText'
import { formatDateTime } from '@/utils/date'

const props = defineProps<{
  config: PublicImportConfig
  groups: AccountGroup[]
  groupsLoading: boolean
}>()
const emit = defineEmits<{
  saved: [config: PublicImportConfig]
  deleted: [id: string]
}>()

// 预设按「保存那一刻起算」；keep 保留已保存的到期时间，custom 由管理员填具体时刻。
const validityOptions = [
  { label: '保持当前', value: 'keep' },
  { label: '1 小时', value: '3600' },
  { label: '24 小时', value: '86400' },
  { label: '7 天', value: '604800' },
  { label: '自定义', value: 'custom' },
  { label: '长期有效', value: 'never' },
]

const copyText = useCopyText()
const saveAction = useAsyncAction()
const rotateAction = useAsyncAction()
const deleteAction = useAsyncAction()

const name = shallowRef('')
const enabled = shallowRef(false)
const pinTurnState = shallowRef(true)
const groupIds = ref<string[]>([])
const token = shallowRef('')
const expiresAt = shallowRef<string | null>(null)
const validity = shallowRef('keep')
const customExpiry = shallowRef('')
const showDeleteModal = shallowRef(false)
const now = shallowRef(Date.now())

const busy = computed(() => saveAction.loading.value || rotateAction.loading.value || deleteAction.loading.value)
const link = computed(() => token.value ? `${window.location.origin}/import/${token.value}` : '')
const expired = computed(() => expiresAt.value !== null && new Date(expiresAt.value).getTime() <= now.value)
const expiryText = computed(() => expiresAt.value === null
  ? '当前：长期有效'
  : `当前：${formatDateTime(expiresAt.value)} ${expired.value ? '已过期，链接不可用' : '到期'}`)
const nextExpiresAt = computed<string | null | undefined>(() => {
  if (validity.value === 'keep')
    return expiresAt.value
  if (validity.value === 'never')
    return null
  if (validity.value === 'custom') {
    const time = new Date(customExpiry.value).getTime()
    return Number.isNaN(time) ? undefined : new Date(time).toISOString()
  }
  return new Date(Date.now() + Number(validity.value) * 1000).toISOString()
})
const nameError = computed(() => name.value.trim().length === 0 ? '请填写号商名称' : undefined)
const expiryError = computed(() => {
  if (nextExpiresAt.value === undefined)
    return '请填写到期时间'
  if (enabled.value && nextExpiresAt.value !== null && new Date(nextExpiresAt.value).getTime() <= Date.now())
    return validity.value === 'keep' ? '链接已过期，请重新选择有效期' : '到期时间必须晚于当前时间'
  return undefined
})
const groupError = computed(() => enabled.value && groupIds.value.length === 0 ? '开启前请至少选择一个目标分组' : undefined)
const canSave = computed(() => !busy.value && !nameError.value && !groupError.value && !expiryError.value)

function apply(config: PublicImportConfig) {
  name.value = config.name
  enabled.value = config.enabled
  pinTurnState.value = config.pinTurnState
  groupIds.value = [...config.groupIds]
  token.value = config.token
  expiresAt.value = config.expiresAt
  validity.value = 'keep'
  customExpiry.value = ''
  now.value = Date.now()
}

watch(() => props.config, config => apply(config), { immediate: true })

async function save() {
  const nextExpiry = nextExpiresAt.value
  if (nameError.value || groupError.value || expiryError.value || nextExpiry === undefined)
    return
  await saveAction.run(async () => {
    const updated = await updatePublicImportConfig({
      id: props.config.id,
      name: name.value.trim(),
      enabled: enabled.value,
      groupIds: groupIds.value,
      pinTurnState: pinTurnState.value,
      expiresAt: nextExpiry,
    })
    apply(updated)
    emit('saved', updated)
    toast.success('已保存')
  })
}

async function rotate() {
  await rotateAction.run(async () => {
    const updated = await rotatePublicImportToken(props.config.id)
    apply(updated)
    emit('saved', updated)
    toast.success('已更换链接，旧链接立即失效')
  })
}

async function remove() {
  await deleteAction.run(async () => {
    await deletePublicImportConfig(props.config.id)
    showDeleteModal.value = false
    emit('deleted', props.config.id)
    toast.success('已删除该号商')
  })
}
</script>

<template>
  <div class="grid gap-4 rounded-cp border border-cp-border bg-cp-fill-quaternary/40 p-4">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <div class="flex min-w-0 items-center gap-2">
        <span class="size-2 rounded-full" :class="enabled && !expired ? 'bg-cp-success-solid' : 'bg-cp-text-quaternary'" />
        <span class="truncate font-heavy text-cp-text">{{ config.name || '未命名号商' }}</span>
        <span v-if="expired" class="text-cp-xs text-cp-error-text">链接已过期</span>
        <span v-else-if="!enabled" class="text-cp-xs text-cp-text-tertiary">未开启</span>
      </div>
      <div class="flex items-center gap-2">
        <BaseButton variant="secondary" size="sm" :disabled="busy" @click="rotate">
          <template #icon>
            <RefreshCw class="size-4" />
          </template>
          更换链接
        </BaseButton>
        <BaseButton variant="primary" size="sm" :loading="saveAction.loading.value" :disabled="!canSave" @click="save">
          <template #icon>
            <Save class="size-4" />
          </template>
          保存
        </BaseButton>
        <BaseIconButton label="删除号商" :disabled="busy" @click="showDeleteModal = true">
          <Trash2 class="size-4 text-cp-error-text" />
        </BaseIconButton>
      </div>
    </div>

    <div class="grid gap-4 md:grid-cols-2">
      <BaseFormItem label="号商名称" required :error="nameError" description="导入的账号会以此为备注，方便追溯是哪个号商丢的号">
        <BaseInput v-model="name" maxlength="60" :disabled="busy" aria-label="号商名称" placeholder="例如：迷茫" />
      </BaseFormItem>
      <BaseFormItem label="开启入口" description="关闭后该号商链接立即不可用，对外表现与链接错误一致">
        <BaseSwitch v-model="enabled" label="开启免登录导入入口" :disabled="busy" />
      </BaseFormItem>
    </div>

    <BaseFormItem label="目标分组" required :error="groupError" description="通过该链接导入的账号会启用并加入这些分组">
      <AccountGroupCheckboxGrid v-model="groupIds" :groups="groups" :loading="groupsLoading" :disabled="busy" />
    </BaseFormItem>

    <BaseFormItem label="自动开启 state 绑定 + WS 保活" description="导入成功后为 OAuth 账号开启「固定自身 state」；开启后 WS 保活暖池会给这些账号预建并挂住满血 WebSocket。API Key 账号不支持，会跳过">
      <BaseSwitch v-model="pinTurnState" label="导入后开启 state 绑定 + WS 保活" :disabled="busy" />
    </BaseFormItem>

    <BaseFormItem label="链接有效期" :error="expiryError" :description="`${expiryText}。预设时长从点击保存时起算；到期后链接自动失效，更换链接不会重置有效期`">
      <div class="grid gap-2">
        <BaseSegmented v-model="validity" label="链接有效期" :options="validityOptions" :disabled="busy" />
        <BaseInput
          v-if="validity === 'custom'"
          v-model="customExpiry"
          type="datetime-local"
          class="max-w-64"
          aria-label="自定义到期时间"
          :disabled="busy"
        />
      </div>
    </BaseFormItem>

    <BaseFormItem label="导入链接" description="出站代理从已通过测试的代理中逐账号随机选择，文件自带的代理会被忽略。持有链接即可导入，请只发给这个号商">
      <div class="flex min-w-0 items-center gap-2">
        <code class="min-w-0 flex-1 rounded-cp bg-cp-fill-quaternary px-3 py-2.5 font-mono text-cp-sm leading-normal font-emphasis break-all text-cp-text">
          {{ link }}
        </code>
        <BaseIconButton size="md" label="复制链接" :disabled="!link" @click="copyText(link, { successText: '已复制链接' })">
          <Copy class="size-4" />
        </BaseIconButton>
      </div>
    </BaseFormItem>

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="删除号商"
      :description="`删除「${config.name || '未命名号商'}」后，它的导入链接立即失效，已发出去的链接不可再用`"
      destructive
      confirm-text="确认删除"
      :loading="deleteAction.loading.value"
      @confirm="remove"
    >
      <p class="m-0">
        确定要删除这个号商吗？
      </p>
    </BaseConfirmModal>
  </div>
</template>
