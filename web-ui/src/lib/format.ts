/**
 * 展示辅助：时间格式化 / JSON 轻量高亮（纯函数，可单测）
 */

/** ISO 时间戳 → 人读（优先相对时间；较旧时回退绝对日期时间） */
export function formatWhen(iso: string, now: number = Date.now()): string {
  const t = Date.parse(iso)
  if (Number.isNaN(t)) return iso
  const diff = now - t
  const min = Math.floor(diff / 60000)
  if (min < 2) return '刚刚'
  if (min < 60) return `${min} 分钟前`
  const h = Math.floor(min / 60)
  if (h < 24) return `${h} 小时前`
  const d = Math.floor(h / 24)
  if (d < 30) return `${d} 天前`
  const dt = new Date(t)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${dt.getFullYear()}-${p(dt.getMonth() + 1)}-${p(dt.getDate())} ${p(dt.getHours())}:${p(dt.getMinutes())}`
}

/** 随机会话 ID（s_ + 8 位十六进制；rng 可注入以便单测） */
export function randomSessionId(rng: () => number = Math.random): string {
  const hex = '0123456789abcdef'
  let out = ''
  for (let i = 0; i < 8; i++) out += hex[Math.floor(rng() * 16)]
  return `s_${out}`
}

/** JSON 轻量语法高亮（输出 HTML，配合 v-html；先转义再着色） */
export function highlightJson(src: string): string {
  const esc = src.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
  return esc.replace(
    /("(?:\\.|[^"\\])*")(\s*:)?|\b(true|false|null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g,
    (m, str?: string, colon?: string, kw?: string) => {
      if (str !== undefined) {
        return colon ? `<span class="j-key">${str}</span>${colon}` : `<span class="j-str">${str}</span>`
      }
      if (kw !== undefined) return `<span class="j-kw">${kw}</span>`
      return `<span class="j-num">${m}</span>`
    },
  )
}