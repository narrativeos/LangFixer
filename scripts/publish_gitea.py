#!/usr/bin/env python3
"""把某标签的发布产物推送到 Gitea 发行版（不存在则创建；已存在的二进制资产跳过，
latest.json 一律重算替换）。

用法: GITEA_TOKEN=<令牌> scripts/publish_gitea.py <tag> <目录>
  例: GITEA_TOKEN=xxx scripts/publish_gitea.py v1.2.0 dist/gitea/v1.2.0

流程（与 publish_atomgit.py 对齐）：
  1. GET  /api/v1/repos/{owner}/{repo}/releases/tags/{tag} 查已有 release；404 则
     POST /api/v1/repos/{owner}/{repo}/releases 创建（tag 需已由 publish.sh 推送）。
  2. 把已有资产（如 release.sh 手工上传的 mac 包与 .sig）下载到发布目录，
     再用 make_latest_json.py 重算 latest.json——清单必须覆盖全部平台键。
  3. 上传新资产：POST /releases/{id}/assets?name=<文件名>，文件字节走请求体；
     同名 latest.json 先 DELETE /releases/assets/{assetId} 再传，其余同名跳过。

API 鉴权头 Authorization: token <令牌>（Gitea 标准，需 write 仓库权限）。
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

BASE = os.environ.get("GITEA_URL", "https://gitea.jiulu.ltd")
API = BASE + "/api/v1"
OWNER = os.environ.get("GITEA_OWNER", "narrativeos")
REPO = os.environ.get("GITEA_REPO", "LangFixer")
HERE = Path(__file__).resolve().parent


def request(url, data=None, method=None, headers=None, timeout=600):
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Accept", "application/json")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def api(path, method="GET", token=None, data=None, headers=None):
    h = dict(headers or {})
    if token:
        h["Authorization"] = f"token {token}"
    return request(API + path, data=data, method=method, headers=h)


def decode(raw):
    try:
        return json.loads(raw.decode("utf-8"))
    except Exception:
        return None


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    tag, src_dir = sys.argv[1], Path(sys.argv[2])
    token = os.environ.get("GITEA_TOKEN", "")
    if not token:
        sys.exit("!! 缺少 GITEA_TOKEN：在 Gitea 个人设置→应用 创建（write:repository）后 export 再运行")
    if not src_dir.is_dir():
        sys.exit(f"!! 目录不存在: {src_dir}")

    # 实例开了匿名隐藏（仓库 404），所有 API 调用统一带令牌
    code, raw = api(f"/repos/{OWNER}/{REPO}/releases/tags/{urllib.parse.quote(tag, '')}", token=token)
    release = decode(raw)
    if code == 200 and release and release.get("tag_name"):
        print(f"==> Gitea 已有 {tag} 的发行版（附件 {len(release.get('assets') or [])} 个）")
    elif code == 404:
        body = json.dumps({
            "tag_name": tag,
            "name": tag,
            "body": "LangFixer {} 发布产物（与 GitHub/AtomGit 同源构建）。"
                    "源码: https://github.com/narrativeos/LangFixer".format(tag),
            "draft": False,
            "prerelease": "-" in tag,
        }).encode()
        code, raw = api(f"/repos/{OWNER}/{REPO}/releases", method="POST",
                        token=token, data=body,
                        headers={"Content-Type": "application/json"})
        release = decode(raw)
        if code not in (200, 201) or not release or not release.get("tag_name"):
            sys.exit(f"!! 创建 release 失败: HTTP {code} {raw[:500]!r}"
                     "（tag 是否已由 scripts/publish.sh 推到 Gitea？）")
        print(f"==> 已创建 Gitea 发行版 {tag}")
    else:
        sys.exit(f"!! 查询 release 失败: HTTP {code} {raw[:500]!r}")

    assets = release.get("assets") or []
    attached = {a.get("name"): a.get("id") for a in assets}

    # 已有资产落盘（不覆盖本次新产物），供 latest.json 合并全部平台键
    for a in assets:
        name, url = a.get("name"), a.get("browser_download_url")
        if not name or not url:
            continue
        dest = src_dir / name
        if dest.exists():
            continue
        # 公开 release 可匿名下；带令牌对私有仓库也兼容
        code, raw = request(url, headers={"Authorization": f"token {token}"})
        if code == 200:
            dest.write_bytes(raw)
            print(f"==> 拉回已有资产 {name}（{len(raw) / 1024 / 1024:.1f} MB）")
        else:
            print(f"!! 拉回 {name} 失败: HTTP {code}（latest.json 可能缺平台键）")

    subprocess.run([sys.executable, str(HERE / "make_latest_json.py"),
                    "gitea", tag, str(src_dir)], check=True)

    release_id = release.get("id")
    for p in sorted(src_dir.iterdir()):
        if not p.is_file():
            continue
        name = p.name
        if name in attached:
            if name != "latest.json":
                print(f"==> 跳过 {name}（已上传）")
                continue
            # 清单必须替换：同名双资产会让下载 URL 歧义，旧清单也会让用户拿不到新版
            code, raw = api(f"/repos/{OWNER}/{REPO}/releases/assets/{attached[name]}",
                             method="DELETE", token=token)
            if code // 100 != 2:
                sys.exit(f"!! 删除旧 {name} 失败: HTTP {code} {raw[:300]!r}")
            print(f"==> 已删除旧 {name}")
        q = urllib.parse.urlencode({"name": name})
        url = f"{API}/repos/{OWNER}/{REPO}/releases/{release_id}/assets?{q}"
        code, raw = request(url, data=p.read_bytes(), method="POST",
                            headers={"Authorization": f"token {token}",
                                     "Content-Type": "application/octet-stream"})
        if code // 100 != 2:
            sys.exit(f"!! 上传 {name} 失败: HTTP {code} {raw[:500]!r}")
        print(f"==> 已上传 {name}（{p.stat().st_size / 1024 / 1024:.1f} MB）")

    print(f"==> 完成: {BASE}/{OWNER}/{REPO}/releases")


if __name__ == "__main__":
    main()
