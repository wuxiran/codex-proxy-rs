import type { AccountCostRow } from '@/api'
import dayjs from 'dayjs'
import { computed, onMounted, shallowRef } from 'vue'
import { getCostAccounts } from '@/api'

/**
 * 账号页只需要「谁录了价、谁已下线」，不需要核算数字；
 * 用当天的单日区间取一次全部购买记录，避免为此扩展账号列表接口。
 */
export function useAccountPurchases() {
  const rows = shallowRef<AccountCostRow[]>([])
  const byAccountId = computed(() => new Map(rows.value.map(row => [row.accountId, row])))

  async function reload() {
    const today = dayjs().format('YYYY-MM-DD')
    try {
      rows.value = (await getCostAccounts({ from: today, to: today, includeRetired: true }, { silent: true })).items
    }
    // 成本信息是辅助展示：取不到时账号页照常工作，只是不显示价格与下线标记。
    catch {}
  }

  onMounted(() => void reload())
  return { byAccountId, reload }
}
