<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api'
import { ListPlus } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { batchCreateProxies } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { parseProxyLines } from '../presenter'

const emit = defineEmits<{
  created: [records: OutboundProxyRecord[]]
}>()
const open = defineModel<boolean>({ required: true })
const input = shallowRef('')
const action = useAsyncAction()
const { loading: saving } = action
const parsed = computed(() => parseProxyLines(input.value))
const MAX_ITEMS = 200
const overLimit = computed(() => parsed.value.valid.length > MAX_ITEMS)

// 文本里有账号密码，弹窗关闭即清空，不在内存里多留。
watch(open, (value) => {
  if (!value)
    input.value = ''
})

async function submit() {
  if (saving.value || parsed.value.valid.length === 0 || overLimit.value)
    return
  await action.run(async () => {
    const result = await batchCreateProxies({ items: parsed.value.valid.map(proxyUrl => ({ proxyUrl })) })
    const skipped = result.skipped.length
    if (result.created.length > 0)
      toast.success(`已添加 ${result.created.length} 条代理${skipped ? `，跳过 ${skipped} 条` : ''}`)
    else
      toast.warning(`没有新增代理，${skipped} 条已存在或地址不合法`)
    open.value = false
    emit('created', result.created)
  })
}
</script>

<template>
  <BaseModal v-model="open" title="批量添加代理" description="每行一条，添加后自动测试连接" size="lg" :dismissible="!saving">
    <BaseForm class="grid gap-4">
      <BaseFormItem label="代理地址" required description="格式：协议://用户名:密码@主机:端口，认证可省略；支持 http、https、socks5、socks5h，IPv6 主机用方括号。">
        <BaseTextarea
          v-model="input"
          :rows="10"
          :disabled="saving"
          resize="vertical"
          class="font-mono"
          aria-label="代理地址，每行一条"
          placeholder="socks5://user:pass@192.0.2.10:1080&#10;http://192.0.2.11:8080"
          spellcheck="false"
          autocomplete="off"
        />
      </BaseFormItem>
      <dl class="m-0 flex flex-wrap gap-x-5 gap-y-1 text-cp-sm" aria-live="polite">
        <div class="flex items-baseline gap-1.5">
          <dt class="text-cp-text-secondary">
            有效
          </dt>
          <dd class="m-0 font-mono font-heavy tabular-nums text-cp-success-text">
            {{ parsed.valid.length }}
          </dd>
        </div>
        <div class="flex items-baseline gap-1.5">
          <dt class="text-cp-text-secondary">
            格式不符
          </dt>
          <dd class="m-0 font-mono font-heavy tabular-nums" :class="parsed.invalid ? 'text-cp-error-text' : 'text-cp-text-tertiary'">
            {{ parsed.invalid }}
          </dd>
        </div>
        <div class="flex items-baseline gap-1.5">
          <dt class="text-cp-text-secondary">
            重复
          </dt>
          <dd class="m-0 font-mono font-heavy tabular-nums" :class="parsed.duplicate ? 'text-cp-warning-text' : 'text-cp-text-tertiary'">
            {{ parsed.duplicate }}
          </dd>
        </div>
      </dl>
      <p v-if="overLimit" class="m-0 text-cp-sm text-cp-error-text">
        一次最多添加 {{ MAX_ITEMS }} 条，请分批提交。
      </p>
      <p class="m-0 text-cp-xs text-cp-text-tertiary">
        名称默认取「主机:端口」，可稍后修改。未通过连接测试的代理不能绑定账号。
      </p>
    </BaseForm>
    <template #footer>
      <BaseButton variant="secondary" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="primary" :loading="saving" :disabled="parsed.valid.length === 0 || overLimit" @click="submit">
        <template #icon>
          <ListPlus class="size-4" />
        </template>
        添加 {{ parsed.valid.length }} 条
      </BaseButton>
    </template>
  </BaseModal>
</template>
