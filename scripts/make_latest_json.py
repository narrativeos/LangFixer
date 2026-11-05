#!/usr/bin/env python3
"""按发布源生成 Tauri updater 更新清单 latest.json。

用法: make_latest_json.py <source> <tag> <dir>

扫描 dir 下的 <artifact>.sig（tauri build 产出，需签名密钥），按文件名映射平台键，
下载 URL 指向该源 release 资产。URL 必须与 desktop-app/src/update.rs 的更新端点一致。
"""
import json
import os
import re
import sys

SOURCE, TAG, DIR = sys.argv[1], sys.argv[2], sys.argv[3]

BASES = {
    "github": "https://github.com/narrativeos/LangFixer/releases/download",
    "gitea": "https://gitea.jiulu.ltd/narrativeos/LangFixer/releases/download",
    # TODO: atomgit 的 release 资产下载 URL 格式待验证，不支持时改用 raw 文件托管
    "atomgit": "https://atomgit.com/futuremeng/LangFixer/releases/download",
}

if SOURCE not in BASES:
    sys.exit(f"!! 未知发布源: {SOURCE!r}（可选: {', '.join(BASES)}）")


def platform_of(sig_name: str):
    name = sig_name.removesuffix(".sig")
    if re.search(r"_aarch64\.dmg$", name):
        return "darwin-aarch64"
    if re.search(r"_x86_64\.dmg$", name):
        return "darwin-x86_64"
    if name.endswith(".AppImage"):
        return "linux-x86_64"
    if re.search(r"_x64-setup\.exe$", name):
        return "windows-x86_64"
    return None


platforms = {}
for f in sorted(os.listdir(DIR)):
    if not f.endswith(".sig"):
        continue
    key = platform_of(f)
    if not key:
        continue
    with open(os.path.join(DIR, f), encoding="utf-8") as fh:
        signature = fh.read().strip()
    platforms[key] = {
        "url": f"{BASES[SOURCE]}/{TAG}/{f.removesuffix('.sig')}",
        "signature": signature,
    }

if not platforms:
    print("!! 目录中没有 .sig（缺少更新签名密钥？），跳过 latest.json")
    sys.exit(0)

manifest = {"version": TAG.lstrip("v"), "notes": "", "platforms": platforms}
path = os.path.join(DIR, "latest.json")
with open(path, "w", encoding="utf-8") as fh:
    json.dump(manifest, fh, indent=2, ensure_ascii=False)
    fh.write("\n")
print(f"==> {path}（平台: {', '.join(platforms)}）")
