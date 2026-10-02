<script setup lang="ts">
import type { PublicImportConfig } from '@/api'
import { Copy, RefreshCw, Save } from '@lucide/vue'
import { computed, onMounted, ref, shallowRef } from 'vue'

import { getPublicImportConfig, rotatePublicImportToken, updatePublicImportConfig } from '@/api'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { toast } from '@/components/base/BaseToast'
import { useAccountGroupCatalog } from '@/composables/useAccountGroupCatalog'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useCopyText } from '@/composables/useCopyText'
import { formatDateTime } from '@/utils/date'

// 预设按「保存那一刻起算」；keep 保留已保存的到期时间，custom 由管理员填具体时刻。
const validityOptions = [
  { label: '保持当前', value: 'keep' },
  { label: '1 小时', value: '3600' },
  { label: '24 小时', value: '86400' },
  { label: '7 天', value: '604800' },
  { label: '自定义', value: 'custom' },
  { label: '长期有效', value: 'never' },
]

const { groups, loading: groupsLoading } = useAccountGroupCatalog()
const copyText = useCopyText()
const saveAction = useAsyncAction()
const rotateAction = useAsyncAction()

const loading = shallowRef(true)
const token = shallowRef('')
const enabled = shallowRef(false)
const pinTurnState = shallowRef(true)
const groupIds = ref<string[]>([])
const showRotateModal = shallowRef(false)
const expiresAt = shallowRef<string | null>(null)
const validity = shallowRef('keep')
const customExpiry = shallowRef('')
// 过期判断只在加载、保存后刷新；页面长时间停留时以服务端校验为准。
const now = shallowRef(Date.now())

const busy = computed(() => loading.value || saveAction.loading.value || rotateAction.loading.value)
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
const expiryError = computed(() => {
  if (nextExpiresAt.value === undefined)
    return '请填写到期时间'
  if (enabled.value && nextExpiresAt.value !== null && new Date(nextExpiresAt.value).getTime() <= Date.now())
    return validity.value === 'keep' ? '链接已过期，请重新选择有效期' : '到期时间必须晚于当前时间'
  return undefined
})
const groupError = computed(() => enabled.value && groupIds.value.length === 0 ? '开启前请至少选择一个目标分组' : undefined)

function apply(config: PublicImportConfig) {
  token.value = config.token
  enabled.value = config.enabled
  pinTurnState.value = config.pinTurnState
  groupIds.value = [...config.groupIds]
  expiresAt.value = config.expiresAt
  validity.value = 'keep'
  customExpiry.value = ''
  now.value = Date.now()
}

onMounted(async () => {
  try {
    apply(await getPublicImportConfig())
  }
  catch {}
  finally {
    loading.value = false
  }
})

async function save() {
  const nextExpiry = nextExpiresAt.value
  if (groupError.value || expiryError.value || nextExpiry === undefined)
    return
  await saveAction.run(async () => {
    apply(await updatePublicImportConfig({ enabled: enabled.value, groupIds: groupIds.value, pinTurnState: pinTurnState.value, expiresAt: nextExpiry }))
    toast.success(enabled.value ? '导入入口已开启' : '导入入口已保存')
  })
}

async function rotate() {
  await rotateAction.run(async () => {
    apply(await rotatePublicImportToken())
    showRotateModal.value = false
    toast.success('已更换链接，旧链接立即失效')
  })
}
</script>

<template>
  <BaseCard title="免登录账号导入" description="把链接发给上游，对方无需账号密码即可导入 sub2api 格式账号">
    <template #actions>
      <div class="flex flex-wrap items-center gap-2">
        <BaseButton variant="secondary" :disabled="busy" @click="showRotateModal = true">
          <template #icon>
            <RefreshCw class="size-4" />
          </template>
          更换链接
        </BaseButton>
        <BaseButton variant="primary" :loading="saveAction.loading.value" :disabled="busy || Boolean(groupError || expiryError)" @click="save">
          <template #icon>
            <Save class="size-4" />
          </template>
          保存
        </BaseButton>
      </div>
    </template>

    <div class="grid max-w-6xl gap-5">
      <BaseFormItem label="开启入口" description="关闭后链接立即不可用，对外表现与链接错误一致">
        <BaseSwitch v-model="enabled" label="开启免登录导入入口" :disabled="busy" />
      </BaseFormItem>

      <BaseFormItem label="目标分组" required :error="groupError" description="通过链接导入的账号会启用并加入这些分组">
        <AccountGroupCheckboxGrid v-model="groupIds" :groups="groups" :loading="groupsLoading" :disabled="busy" />
      </BaseFormItem>

      <BaseFormItem label="自动开启 state 绑定" description="导入成功后为 OAuth 账号开启「固定自身 state」；API Key 账号不支持，会跳过">
        <BaseSwitch v-model="pinTurnState" label="导入后自动开启 state 绑定" :disabled="busy" />
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

      <BaseFormItem label="导入链接" description="出站代理从已通过测试的代理中逐账号随机选择，文件自带的代理会被忽略。持有链接即可导入，请只发给可信的上游">
        <div class="flex min-w-0 items-center gap-2">
          <code class="min-w-0 flex-1 rounded-cp bg-cp-fill-quaternary px-3 py-2.5 font-mono text-cp-sm leading-normal font-emphasis break-all text-cp-text">
            {{ loading ? '加载中...' : link }}
          </code>
          <BaseIconButton size="md" label="复制链接" :disabled="!link" @click="copyText(link, { successText: '已复制链接' })">
            <Copy class="size-4" />
          </BaseIconButton>
        </div>
      </BaseFormItem>
    </div>

    <BaseConfirmModal
      v-model="showRotateModal"
      title="更换导入链接"
      description="旧链接会立即失效，已发出去的链接需要重新发送"
      destructive
      confirm-text="确认更换"
      :loading="rotateAction.loading.value"
      @confirm="rotate"
    >
      <p class="m-0">
        确定要更换免登录导入链接吗？
      </p>
    </BaseConfirmModal>
  </BaseCard>
</template>
