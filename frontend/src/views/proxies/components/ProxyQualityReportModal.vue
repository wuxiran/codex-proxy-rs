<script setup lang="ts">
import type { OutboundProxyRecord, ProxyQualityItem, ProxyQualityReport } from '@/api'
import { ShieldCheck } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { getProxyQualityReport } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { formatDateTime } from '@/utils/date'
import { exitGeoLabel, qualityItemStatus, qualityStatus, qualityTargetLabel } from '../presenter'

const props = defineProps<{
  proxy: OutboundProxyRecord | null
  // 刚完成的检测直接带入报告，省一次往返；从列表徽标打开时为空，按需读取。
  report: ProxyQualityReport | null
  checking: boolean
}>()
const emit = defineEmits<{
  recheck: [proxy: OutboundProxyRecord]
}>()
const open = defineModel<boolean>({ required: true })
const loaded = shallowRef<ProxyQualityReport | null>(null)
const loading = shallowRef(false)
const failed = shallowRef(false)
let controller: AbortController | undefined

const current = computed(() => props.report ?? loaded.value)
const status = computed(() => current.value ? qualityStatus(current.value.status) : null)
const columns = defineTableColumns<ProxyQualityItem>([
  { key: 'target', label: '检测项', kind: 'custom', size: 'lg' },
  { key: 'status', label: '状态', kind: 'custom', size: 'xs' },
  { key: 'httpStatus', label: 'HTTP', kind: 'custom', size: 'xs' },
  { key: 'latencyMs', label: '延迟', kind: 'custom', size: 'xs' },
  { key: 'message', label: '说明', kind: 'custom', size: 'xl' },
])

async function load() {
  const proxy = props.proxy
  controller?.abort()
  if (!proxy || props.report)
    return
  controller = new AbortController()
  const signal = controller.signal
  loading.value = true
  failed.value = false
  try {
    const result = await getProxyQualityReport({ id: proxy.id }, { signal, silent: true })
    if (!signal.aborted)
      loaded.value = result.report
  }
  catch {
    if (!signal.aborted)
      failed.value = true
  }
  finally {
    if (!signal.aborted)
      loading.value = false
  }
}

watch([open, () => props.proxy?.id], ([isOpen]) => {
  loaded.value = null
  failed.value = false
  if (isOpen)
    void load()
  else
    controller?.abort()
})
</script>

<template>
  <BaseModal v-model="open" title="代理质量检测报告" :description="proxy?.name" size="lg" tone="info">
    <template #icon>
      <ShieldCheck class="size-5 text-cp-primary-text" :stroke-width="1.75" />
    </template>

    <div aria-live="polite" :aria-busy="loading || checking">
      <div v-if="loading || checking" class="grid gap-3">
        <BaseSkeleton class="h-24 w-full" />
        <BaseSkeleton class="h-40 w-full" />
      </div>
      <BaseEmpty v-else-if="failed" title="报告加载失败" description="暂时无法读取检测报告，请重新加载。" surface="none" class="min-h-64 content-center">
        <template #action>
          <BaseButton variant="secondary" @click="load">
            重新加载
          </BaseButton>
        </template>
      </BaseEmpty>
      <BaseEmpty v-else-if="!current" title="还没有检测报告" description="运行一次质量检测，查看该代理访问各上游的情况。" :icon="ShieldCheck" surface="none" class="min-h-64 content-center" />
      <div v-else class="grid gap-4">
        <section class="grid gap-4 rounded-cp-card bg-cp-fill-quaternary p-4 sm:grid-cols-[auto_minmax(0,1fr)] sm:items-center">
          <div class="flex items-baseline gap-2 sm:grid sm:justify-items-center sm:gap-1 sm:px-3">
            <strong class="font-mono text-4xl leading-none font-heavy tabular-nums text-cp-text">{{ current.score }}</strong>
            <span class="flex items-center gap-1.5">
              <span class="text-cp-sm font-heavy text-cp-text-secondary">等级 {{ current.grade }}</span>
              <span v-if="status" class="rounded-cp px-1.5 py-0.5 text-cp-xs font-bold" :class="status.badge">{{ status.label }}</span>
            </span>
          </div>
          <dl class="m-0 grid grid-cols-2 gap-x-4 gap-y-2 text-cp-xs">
            <div class="min-w-0">
              <dt class="text-cp-text-tertiary">
                出口 IP
              </dt>
              <dd class="m-0 mt-0.5 break-all font-mono text-cp-sm text-cp-text">
                {{ current.exitIp ?? '—' }}
              </dd>
            </div>
            <div class="min-w-0">
              <dt class="text-cp-text-tertiary">
                出口地区
              </dt>
              <dd class="m-0 mt-0.5 flex min-w-0 items-center gap-1.5 text-cp-sm text-cp-text">
                <template v-if="current.exitGeo">
                  <span class="shrink-0 rounded-cp-sm bg-cp-fill-secondary px-1 font-mono text-[10px] leading-4 font-bold text-cp-text-secondary">{{ current.exitGeo.countryCode }}</span>
                  <span class="truncate">{{ exitGeoLabel(current.exitGeo) }}</span>
                </template>
                <template v-else>
                  —
                </template>
              </dd>
            </div>
            <div>
              <dt class="text-cp-text-tertiary">
                基础延迟
              </dt>
              <dd class="m-0 mt-0.5 font-mono text-cp-sm tabular-nums text-cp-text">
                {{ current.baseLatencyMs === null ? '—' : `${current.baseLatencyMs} ms` }}
              </dd>
            </div>
            <div>
              <dt class="text-cp-text-tertiary">
                检测时间
              </dt>
              <dd class="m-0 mt-0.5 font-mono text-cp-sm tabular-nums text-cp-text">
                {{ formatDateTime(current.checkedAt) }}
              </dd>
            </div>
          </dl>
        </section>

        <BaseTable :columns="columns" :rows="current.items" row-key="target" density="compact" empty-text="没有检测项">
          <template #target="{ row }">
            <span class="text-cp-sm font-emphasis text-cp-text">{{ qualityTargetLabel(row.target) }}</span>
          </template>
          <template #status="{ row }">
            <span class="rounded-cp px-1.5 py-0.5 text-cp-xs font-bold" :class="qualityItemStatus(row.status).badge">
              {{ qualityItemStatus(row.status).label }}
            </span>
          </template>
          <template #httpStatus="{ row }">
            <span class="font-mono text-cp-xs tabular-nums">{{ row.httpStatus ?? '—' }}</span>
          </template>
          <template #latencyMs="{ row }">
            <span class="font-mono text-cp-xs tabular-nums">{{ row.latencyMs === null ? '—' : `${row.latencyMs} ms` }}</span>
          </template>
          <template #message="{ row }">
            <span class="text-cp-xs text-cp-text-secondary">
              {{ row.message }}<span v-if="row.cfRay" class="font-mono text-cp-text-tertiary">（cf-ray: {{ row.cfRay }}）</span>
            </span>
          </template>
        </BaseTable>

        <p class="m-0 text-cp-xs leading-relaxed text-cp-text-tertiary">
          检测用无凭据请求判断各上游是否可达：返回 401 等预期状态即视为通过；命中 Cloudflare 挑战说明该出口大概率会被上游拦截。
        </p>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="secondary" @click="open = false">
        关闭
      </BaseButton>
      <BaseButton v-if="proxy" variant="primary" :loading="checking" :disabled="loading" @click="emit('recheck', proxy)">
        <template #icon>
          <ShieldCheck class="size-4" />
        </template>
        重新检测
      </BaseButton>
    </template>
  </BaseModal>
</template>
