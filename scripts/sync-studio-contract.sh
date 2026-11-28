#!/usr/bin/env bash
# Vendor the studio-sidecar metric contract from narrative-docs into
# ./contract. The narrative-docs file is the single source of truth; never
# edit the vendored copy in place. Run it whenever the contract's
# contract_version changes. gec-zh's mirror test
# (gec-zh/tests/studio_contract_mirror.rs) reads the vendored copy.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="${1:-$here/../narrative-docs/architecture/studio-sidecar/metrics.v1.json}"
dest="$here/contract/studio-metrics.v1.json"
if [ ! -f "$src" ]; then
  echo "contract source not found: $src" >&2
  echo "expected narrative-docs to sit next to LangFixer, or pass its path as $0's first argument" >&2
  exit 1
fi
mkdir -p "$(dirname "$dest")"
cp "$src" "$dest"
grep -o '"contract_version"[[:space:]]*:[[:space:]]*"[^"]*"' "$dest" | head -1
echo "synced contract -> $dest"
