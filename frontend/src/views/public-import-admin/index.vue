<script setup lang="ts">
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import PublicImportCard from '@/views/settings/components/PublicImportCard.vue'
</script>

<template>
  <div class="w-full">
    <BasePageHeader
      title="免登录导入"
      description="生成免登录导入链接，上游无需账号密码即可导入 sub2api 格式账号"
    />
    <div class="mt-5 grid w-full gap-5">
      <PublicImportCard />
    </div>
  </div>
</template>
