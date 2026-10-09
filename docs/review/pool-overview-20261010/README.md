# 票池总览页面审查证据

页面入口：`/turn-state`，桌面 1440×1100，窄屏 390×844

截图来自本地实际运行的生产构建，使用合成账号与模拟 API，不代表线上账号、库存或模型质量
变更前基线为 `a6d8af76495893d095d83b67cfb0bc5d6a9529b9`，变更后对应本 PR 的页面实现

- [变更前](before.png)
- [浅色总览](after-light.png)
- [深色总览](after-desktop-dark.png)
- [网关详情](after-detail.png)
- [移动端深色](after-mobile-dark.png)
- [加载状态](after-loading.png)
- [读取失败与过期](after-error.png)
- [空状态](after-empty.png)

浏览器验证覆盖搜索、筛选、键盘打开详情、Escape 关闭、失焦暂停、读取失败、过期失效、空池、错误恢复及窄屏无横向溢出
最终运行无页面 JavaScript 异常，票池浏览过程没有发起写请求
