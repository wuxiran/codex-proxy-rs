<script setup lang="ts">
// fork: proxy-copy — 代理地址复制菜单（完整 URL / host:port），只复制脱敏端点。
import { ZIconButton, ZMenuItem, ZPopover } from '@codex-proxy/ui'
import { Copy } from '@lucide/vue'
import { useCopyText } from '@/composables/useCopyText'
import { endpointCopyFormats } from '../presenter.fork'

defineProps<{
  endpoint: string
  hasAuthentication: boolean
}>()

const copyText = useCopyText()

async function copyEndpoint(value: string, close: () => void) {
  close()
  await copyText(value, { successText: '代理地址已复制' })
}
</script>

<template>
  <ZPopover placement="bottom-start">
    <template #reference="{ open }">
      <ZIconButton size="small" aria-label="复制代理地址" aria-haspopup="menu" :pressed="open">
        <Copy class="size-3" />
      </ZIconButton>
    </template>
    <template #default="{ close }">
      <div role="menu" class="grid w-64 gap-0.5 p-1.5">
        <ZMenuItem v-for="format in endpointCopyFormats(endpoint)" :key="format.value" role="menuitem" @click="copyEndpoint(format.value, close)">
          <span class="truncate font-mono text-cp-xs">{{ format.label }}</span>
        </ZMenuItem>
        <p v-if="hasAuthentication" class="m-0 px-3 pt-1 pb-1.5 text-cp-xs leading-relaxed text-cp-text-tertiary">
          账号密码只保存在服务端，不会出现在复制内容里。
        </p>
      </div>
    </template>
  </ZPopover>
</template>
