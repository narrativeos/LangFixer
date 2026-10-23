#!/bin/zsh
# 下载 CEC 4B GGUF（Q8_0 近无损）：hf-mirror 镜像优先，huggingface.co 兜底
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
dl() {
  local dest="$1"; shift
  if [ -s "$dest" ] && [ "$(stat -f%z "$dest")" -gt 4000000000 ]; then
    echo "skip $dest (already complete)"; return 0
  fi
  for url in "$@"; do
    echo "trying $url"
    if curl -fL --retry 2 -C - -o "$dest" "$url"; then
      echo "OK $dest from $url"; return 0
    fi
    echo "fail: $url"
  done
  echo "FAILED $dest"; return 1
}
dl $ROOT/models/cec4.Q8_0.gguf \
  'https://hf-mirror.com/mradermacher/ChineseErrorCorrector4-4B-GGUF/resolve/main/ChineseErrorCorrector4-4B.Q8_0.gguf' \
  'https://huggingface.co/mradermacher/ChineseErrorCorrector4-4B-GGUF/resolve/main/ChineseErrorCorrector4-4B.Q8_0.gguf'
dl $ROOT/models/cec3.Q8_0.gguf \
  'https://hf-mirror.com/mradermacher/ChineseErrorCorrector3-4B-GGUF/resolve/main/ChineseErrorCorrector3-4B.Q8_0.gguf' \
  'https://huggingface.co/mradermacher/ChineseErrorCorrector3-4B-GGUF/resolve/main/ChineseErrorCorrector3-4B.Q8_0.gguf'
echo ALL_DONE