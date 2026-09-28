import type { RouteRecordRaw } from 'vue-router'

/** fork 的顶层页面。 */
export const forkPublicRoutes: RouteRecordRaw[] = [
  {
    // 免登录密链页面；令牌在路径里，接口调用时改放请求头。
    path: '/import/:token',
    name: 'public-import',
    component: () => import('@/views/public-import/index.vue'),
  },
]

/** fork 的管理端页面，挂在管理布局下。 */
export const forkAdminRoutes: RouteRecordRaw[] = [
  {
    // 免登录导入管理页（生成/轮换密链）；密链持有者面向的页面是 /import/:token。
    path: 'public-import',
    name: 'public-import-admin',
    component: () => import('@/views/public-import-admin/index.vue'),
  },
  {
    path: 'ops-report',
    name: 'ops-report',
    component: () => import('@/views/ops-report/index.vue'),
  },
  {
    path: 'logs',
    name: 'logs',
    component: () => import('@/views/logs/index.vue'),
  },
  {
    path: 'testbench',
    name: 'testbench',
    component: () => import('@/views/testbench/index.vue'),
  },
  {
    path: 'turn-state',
    name: 'turn-state',
    component: () => import('@/views/turn-state/index.vue'),
  },
]
