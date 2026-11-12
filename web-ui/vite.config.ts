import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  server: {
    port: 4805,
    // 显式绑定 IPv4：默认 `localhost` 在本机只解析到 ::1（IPv6），
    // 而 scripts/ensure-vite.sh 的健康检查走 127.0.0.1，会误判启动失败
    host: '127.0.0.1',
    // 业务面 + 验证面都转发到 api-server（门禁页要打通 /auth/whoami 与 /admin/*）
    proxy: {
      '/gec': { target: 'http://localhost:4801', changeOrigin: true },
      '/auth': { target: 'http://localhost:4801', changeOrigin: true },
      '/admin': { target: 'http://localhost:4801', changeOrigin: true },
    },
  },
})
