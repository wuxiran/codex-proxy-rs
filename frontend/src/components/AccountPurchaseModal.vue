<script setup lang="ts">
import type { AccountPurchase } from '@/api'
import { Save } from '@lucide/vue'
import dayjs from 'dayjs'
import { computed, shallowRef, watch } from 'vue'
import { setAccountPurchase, setAccountsRetired } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'

const props = defineProps<{
  account: { id: string, label: string, addedAt?: string | null } | null
  purchase: AccountPurchase | null
}>()
const emit = defineEmits<{ saved: [] }>()
const open = defineModel<boolean>({ required: true })

const DATETIME_LOCAL = 'YYYY-MM-DDTHH:mm'
const price = shallowRef('')
const purchasedAt = shallowRef('')
const note = shallowRef('')
const retired = shallowRef(false)
const action = useAsyncAction()
const { loading: saving } = action

// 价格最多两位小数；留空表示不录价（只标记下线也允许）。
const parsedPrice = computed(() => {
  const text = price.value.trim()
  if (!text)
    return null
  return /^\d{1,10}(?:\.\d{1,2})?$/.test(text) ? Number(text) : Number.NaN
})
const priceError = computed(() => Number.isNaN(parsedPrice.value) ? '请输入不超过两位小数的金额' : undefined)

watch(open, (value) => {
  if (!value)
    return
  price.value = props.purchase?.price == null ? '' : String(props.purchase.price)
  purchasedAt.value = dayjs(props.purchase?.purchasedAt ?? props.account?.addedAt ?? undefined).format(DATETIME_LOCAL)
  note.value = props.purchase?.note ?? ''
  retired.value = props.purchase?.retiredAt != null
})

async function save() {
  const account = props.account
  if (!account || saving.value || priceError.value)
    return
  await action.run(async () => {
    const purchased = dayjs(purchasedAt.value)
    await setAccountPurchase({
      accountId: account.id,
      price: parsedPrice.value,
      purchasedAt: purchased.isValid() ? purchased.toISOString() : undefined,
      note: note.value.trim() || null,
    })
    if (retired.value !== (props.purchase?.retiredAt != null))
      await setAccountsRetired({ accountIds: [account.id], retired: retired.value })
    toast.success('成本信息已保存')
    open.value = false
    emit('saved')
  })
}
</script>

<template>
  <BaseModal v-model="open" title="成本与下线" :description="account?.label" size="sm" :dismissible="!saving">
    <BaseForm class="grid gap-5">
      <BaseFormItem label="购买价格" :error="priceError" description="与跑出的美元金额按 1:1 对比；留空表示未录价。">
        <BaseInput v-model="price" inputmode="decimal" :disabled="saving" aria-label="购买价格" placeholder="例如 51.50" />
      </BaseFormItem>
      <BaseFormItem label="购买时间" description="投入记在这一天；默认取账号加入时间。">
        <BaseInput v-model="purchasedAt" type="datetime-local" :disabled="saving" aria-label="购买时间" />
      </BaseFormItem>
      <BaseFormItem label="备注">
        <BaseInput v-model="note" maxlength="512" :disabled="saving" aria-label="备注" placeholder="批次、来源等" />
      </BaseFormItem>
      <div class="grid gap-1.5">
        <BaseSwitch v-model="retired" label="已下线" show-label :disabled="saving" />
        <p class="m-0 text-cp-xs leading-relaxed text-cp-text-tertiary">
          下线只是标记：账号列表和成本核算默认不再显示它，但不会停用账号，历史核算也照常计入。号掉线后重新上来，取消下线即可。
        </p>
      </div>
    </BaseForm>
    <template #footer>
      <BaseButton variant="secondary" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="primary" :loading="saving" :disabled="!!priceError" @click="save">
        <template #icon>
          <Save class="size-4" />
        </template>
        保存
      </BaseButton>
    </template>
  </BaseModal>
</template>
