/**
 * 错误消息提取（纯函数，可单测）
 *
 * 背景：Tauri 命令失败时 `invoke` 的 Promise 以**字符串** reject
 * （Rust 侧 `Result<_, String>` 的错误信息本身），而 HTTP 路径 reject
 * 的是 `AxiosError`（有 `message`）。直接 `(e as Error).message` 在
 * Tauri 路径会得到 `undefined`（UI 显示「…失败：undefined」）。
 */
export function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message
  if (typeof e === 'string') return e
  if (e == null) return '未知错误'
  try {
    return JSON.stringify(e)
  } catch {
    return String(e)
  }
}

/** 是否为「主动取消」错误（axios AbortSignal 取消：ERR_CANCELED / CanceledError）。
 *  取消不是故障：调用方应静默回到空闲态，不写 errorMsg、不弹提示。 */
export function isCancelError(e: unknown): boolean {
  if (e == null || typeof e !== 'object') return false
  const err = e as { code?: string; name?: string }
  return err.code === 'ERR_CANCELED' || err.name === 'CanceledError'
}
