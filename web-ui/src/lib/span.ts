/**
 * span 口径转换（全库唯一实现）：检测产出的 span（错误项、句表块）一律是
 * UTF-8 字节偏移（与 Rust 侧切片口径一致），前端一切按位置消费文本的代码
 * 必须先转 UTF-16 索引，否则 CJK 文本上预览/计数成倍错位。
 * 末尾哨兵 set(总字节数, text.length)：span 端点落在文本末尾时也可换算。
 */
export function byteToU16Map(text: string): Map<number, number> {
  const m = new Map<number, number>()
  let byte = 0
  for (let u = 0; u < text.length; ) {
    m.set(byte, u)
    const cp = text.codePointAt(u) ?? 0
    byte += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4
    u += cp > 0xffff ? 2 : 1
  }
  m.set(byte, text.length)
  return m
}
