import { createApp } from 'vue'
import { createPinia } from 'pinia'
import ElementPlus from 'element-plus'
import 'element-plus/dist/index.css'
import 'element-plus/theme-chalk/dark/css-vars.css'
import * as ElementPlusIconsVue from '@element-plus/icons-vue'
import App from './App.vue'
import './style.css'

// 桌面端（Tauri）环境标记：供 style.css 的 .desktop 自适应布局使用
// （应用壳式全高布局 / 全宽内容区 / 检测页双独立滚动列）
if (
  typeof window !== 'undefined' &&
  (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__
) {
  document.documentElement.classList.add('desktop')
}

// 主题（日/夜）：localStorage 优先，其次系统 prefers-color-scheme；
// html.dark 时 Element Plus 暗色变量与 style.css 的暗色 token 同时生效（App header 可切换并持久化）
try {
  const saved = localStorage.getItem('langfixer.theme')
  const dark = saved ? saved === 'dark' : window.matchMedia('(prefers-color-scheme: dark)').matches
  if (dark) document.documentElement.classList.add('dark')
} catch {
  /* localStorage / matchMedia 不可用：保持浅色 */
}

const app = createApp(App)
app.use(createPinia())
app.use(ElementPlus)
for (const [key, component] of Object.entries(ElementPlusIconsVue)) {
  app.component(key, component)
}
app.mount('#app')
