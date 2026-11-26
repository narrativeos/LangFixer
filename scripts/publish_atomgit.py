#!/usr/bin/env python3
"""把某标签的发布产物推送到 AtomGit 发行版（不存在则创建；同名附件已传则跳过）。

用法: ATOMGIT_TOKEN=<私人令牌> scripts/publish_atomgit.py <tag> <目录>
  例: ATOMGIT_TOKEN=xxx scripts/publish_atomgit.py v1.1.0 dist/atomgit/v1.1.0

AtomGit 不支持 git push 发布 release，走 api.atomgit.com 的 v5 REST（Gitee 兼容）：
  GET  /repos/{owner}/{repo}/releases/tags/{tag}        查已有 release
  POST /repos/{owner}/{repo}/releases?access_token=…    创建 release
  GET  /repos/{owner}/{repo}/releases/{tag}/upload_url?file_name=…  取附件上传地址
  PUT  {url} + 返回的 headers                           上传附件
文档: https://atomgit-api-doc.atomgit.com/14.release/overview.html
"""
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

API = "https://api.atomgit.com/api/v5"
OWNER = os.environ.get("ATOMGIT_OWNER", "futuremeng")
REPO = os.environ.get("ATOMGIT_REPO", "LangFixer")


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


def api(path, method="GET", headers=None, data=None):
    return request(API + path, data=data, method=method, headers=headers)


def decode(raw):
    try:
        return json.loads(raw.decode("utf-8"))
    except Exception:
        return None


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    tag, src_dir = sys.argv[1], Path(sys.argv[2])
    token = os.environ.get("ATOMGIT_TOKEN", "")
    if not token:
        sys.exit("!! 缺少 ATOMGIT_TOKEN：在 atomgit.com 个人设置→私人令牌 创建后 export 再运行")
    if not src_dir.is_dir():
        sys.exit(f"!! 目录不存在: {src_dir}")
    files = sorted(p for p in src_dir.iterdir() if p.is_file())
    if not files:
        sys.exit(f"!! 目录为空: {src_dir}")

    # tag 带后缀（-rc1/-preview）按预发布处理，与 GitHub 的 prerelease 判定一致
    status_field = "pre" if "-" in tag else "latest"

    code, raw = api(f"/repos/{OWNER}/{REPO}/releases/tags/{urllib.parse.quote(tag, '')}")
    release = decode(raw)
    if code == 200 and release and release.get("tag_name"):
        print(f"==> AtomGit 已有 {tag} 的发行版（附件 {len(release.get('assets') or [])} 个）")
    elif code == 404:
        body = json.dumps({
            "tag_name": tag,
            "name": tag,
            "body": "LangFixer {} 发布产物（与 GitHub/Gitea 同源构建）。"
                    "源码: https://github.com/narrativeos/LangFixer".format(tag),
            "release_status": status_field,
        }).encode()
        url = f"{API}/repos/{OWNER}/{REPO}/releases?access_token={urllib.parse.quote(token, '')}"
        code, raw = request(url, data=body, method="POST",
                            headers={"Content-Type": "application/json"})
        release = decode(raw)
        if code != 200 or not release or not release.get("tag_name"):
            sys.exit(f"!! 创建 release 失败: HTTP {code} {raw[:500]!r}")
        print(f"==> 已创建 AtomGit 发行版 {tag}")
    else:
        sys.exit(f"!! 查询 release 失败: HTTP {code} {raw[:500]!r}")

    attached = {a.get("name") for a in (release.get("assets") or [])}
    for p in files:
        name = p.name
        if name in attached:
            print(f"==> 跳过 {name}（已上传）")
            continue
        q = urllib.parse.urlencode({"access_token": token, "file_name": name})
        url = f"{API}/repos/{OWNER}/{REPO}/releases/{urllib.parse.quote(tag, '')}/upload_url?{q}"
        code, raw = request(url)
        info = decode(raw)
        if code != 200 or not info or not info.get("url"):
            sys.exit(f"!! 获取 {name} 上传地址失败: HTTP {code} {raw[:500]!r}")
        data = p.read_bytes()
        headers = dict(info.get("headers") or {})
        headers["Content-Length"] = str(len(data))
        code, raw = request(info["url"], data=data, method="PUT", headers=headers)
        if code // 100 != 2:
            sys.exit(f"!! 上传 {name} 失败: HTTP {code} {raw[:500]!r}")
        print(f"==> 已上传 {name}（{len(data) / 1024 / 1024:.1f} MB）")

    print(f"==> 完成: https://atomgit.com/{OWNER}/{REPO}/releases")


if __name__ == "__main__":
    main()
