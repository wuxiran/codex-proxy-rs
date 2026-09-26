<script setup lang="ts">
import type { PublicImportConfig } from '@/api'
import { Plus } from '@lucide/vue'
import { onMounted, ref, shallowRef } from 'vue'

import { createPublicImportConfig, listPublicImportConfigs } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import { toast } from '@/components/base/BaseToast'
import { useAccountGroupCatalog } from '@/composables/useAccountGroupCatalog'
import { useAsyncAction } from '@/composables/useAsyncAction'
import PublicImportSupplierCard from './PublicImportSupplierCard.vue'

const { groups, loading: groupsLoading } = useAccountGroupCatalog()
const createAction = useAsyncAction()

const loading = shallowRef(true)
const configs = ref<PublicImportConfig[]>([])

onMounted(async () => {
  try {
    configs.value = (await listPublicImportConfigs()).configs
  }
  catch {}
  finally {
    loading.value = false
  }
})

async function add() {
  await createAction.run(async () => {
    const created = await createPublicImportConfig({
      name: '新号商',
      enabled: false,
      groupIds: [],
      pinTurnState: true,
      expiresAt: null,
    })
    configs.value = [...configs.value, created]
    toast.success('已新增号商，填写名称与目标分组后开启')
  })
}

function onSaved(updated: PublicImportConfig) {
  configs.value = configs.value.map(config => (config.id === updated.id ? updated : config))
}

function onDeleted(id: string) {
  configs.value = configs.value.filter(config => config.id !== id)
}
</script>

<template>
  <BaseCard
    title="免登录账号导入"
    description="每个上游号商一个独立链接，对方无需账号密码即可导入 sub2api 格式账号；导入的账号会带上号商名，便于在账号列表追溯来源"
  >
    <template #actions>
      <BaseButton variant="primary" :loading="createAction.loading.value" :disabled="loading" @click="add">
        <template #icon>
          <Plus class="size-4" />
        </template>
        新增号商
      </BaseButton>
    </template>

    <div class="grid gap-4">
      <p v-if="loading" class="m-0 text-cp-sm text-cp-text-tertiary">
        加载中...
      </p>
      <p
        v-else-if="configs.length === 0"
        class="m-0 rounded-cp border border-dashed border-cp-border px-4 py-8 text-center text-cp-sm text-cp-text-tertiary"
      >
        还没有号商，点「新增号商」为每个上游号商创建一个独立的导入链接。
      </p>
      <PublicImportSupplierCard
        v-for="config in configs"
        :key="config.id"
        :config="config"
        :groups="groups"
        :groups-loading="groupsLoading"
        @saved="onSaved"
        @deleted="onDeleted"
      />
    </div>
  </BaseCard>
</template>
