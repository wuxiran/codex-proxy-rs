import type { Component } from 'vue'

import { Activity, FlaskConical, ScrollText, TrendingUp, Upload } from '@lucide/vue'

interface NavItem {
  label: string
  icon: Component
  path: string
}

/** fork 菜单项及其位置：排在 `after` 指向的上游菜单项之后。 */
const forkNavItems: { after: string, items: NavItem[] }[] = [
  {
    after: '/accounts',
    items: [{ label: '免登录导入', icon: Upload, path: '/public-import' }],
  },
  {
    after: '/usage',
    items: [
      { label: '经营日报', icon: TrendingUp, path: '/ops-report' },
      { label: '请求日志', icon: ScrollText, path: '/logs' },
      { label: '测智台', icon: FlaskConical, path: '/testbench' },
      { label: '票据管理', icon: Activity, path: '/turn-state' },
    ],
  },
]

/** 把 fork 菜单项原地插入上游菜单；锚点被上游改名时排到插件菜单之前。 */
export function insertForkNavItems(navItems: NavItem[]) {
  for (const { after, items } of forkNavItems) {
    const anchor = navItems.findIndex(item => item.path === after)
    const fallback = navItems.findIndex(item => item.path === '/plugins')
    const index = anchor >= 0 ? anchor + 1 : fallback >= 0 ? fallback : navItems.length
    navItems.splice(index, 0, ...items)
  }
}
