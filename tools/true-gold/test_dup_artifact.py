#!/usr/bin/env python3
"""dup_artifact 校验测试：合成用例 + ~/tmp/dataset_v22 抽检实证 iids。"""
import importlib.util
import sys
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "merge", Path(__file__).resolve().parent / "merge.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

dup = m.dup_artifact
fails = []


def expect(src, a, b, fix, want, tag):
    got = dup(src, a, b, fix)
    if want and got is None or (not want and got is not None and
                                not (len(fix) == 1 and fix in m.REDUP_CHARS)):
        fails.append(f"{tag}: src={src!r} a={a} b={b} fix={fix!r} → {got!r}")


# 合成：后侧复读
expect("不但能帮助政府", 1, 1, "不", True, "backward-1")
expect("2022年1月1日是新医保目录实施的", 4, 4, "022", True, "backward-3")
expect("散文句", 1, 1, "文句", True, "forward-2")          # 右侧邻接复读
expect("现在情况尚未见底", 1, 1, "现", True, "backward-现")
expect("长城东一路口往进城", 3, 3, "一路口", True, "forward-3")
expect("在她身边照顾她再做功德做", 5, 12, "照顾她再做功德做", True, "prefix照")
# 合成：合法编辑不判
expect("呈现人上网时数激增", 2, 3, "出人均", False, "replace-人均")
expect("一定要认真对待", 7, 7, "", False, "delete-only")
expect("创造一个良好交流的环境", 7, 9, "的交流", False, "replace-的")
# 白名单：单字重叠词不误剔（无白名单则本例会误判 backward-1）
for c in m.REDUP_CHARS:
    if dup(c + "变好", 1, 1, c) is not None:
        fails.append(f"redup whitelist broke: {c}")

# 真实数据：抽检确认为复读残片的 iids 必须全部命中
V22 = Path.home() / "tmp/dataset_v22"
ART_IIDS = [
    "cscd-1054#1#1", "cscd-1249#1#1", "cscdns-19462#10#10",
    "midu-3368#59#59", "midu-3400#4#4", "mucgec-10922#1#1",
    "nacgec-17069#60#60", "sig13-4663#1#1", "sig14-5750#31#51",
    "cscdns-17799#36#36", "cscdns-21915#48#48", "ctc21-4138#34#34",
    "nacgec-13092#25#25", "nacgec-16822#41#41", "nacgec-17013#11#11",
    "mucgec-10784#26#26",
]
# 抽检 adopt 的 iids 必须全部不命中（抽查代表）
ADOPT_IIDS = [
    "cscd-1259#12#13", "cctc-8898#10#10", "cscdns-20453#38#38",
    "nacgec-15277#20#20", "nacgec-15469#38#38", "sig14-5930#14#31",
    "nacgec-16490#20#26",
]
if V22.exists():
    rows = m.load_rows(V22, "v22")

    def scan(iids, want, tag):
        for iid in iids:
            rid, ab = iid.split("#", 1)
            a, b = ab.split("#")
            a, b = int(a), int(b)
            r = rows.get(rid)
            if r is None:
                fails.append(f"{tag} {iid}: 行不存在")
                continue
            hit = [e for e in r["model_errors"] if e["a"] == a and e["b"] == b]
            if not hit:
                fails.append(f"{tag} {iid}: 编辑不存在")
                continue
            got = dup(r["src"], a, b, hit[0].get("fix", ""))
            if want and got is None:
                fails.append(f"{tag} {iid}: 未命中 fix={hit[0].get('fix')!r} "
                             f"src={r['src'][max(0,a-6):b+6]!r}")
            if not want and got is not None:
                fails.append(f"{tag} {iid}: 误判 dup={got!r}")

    scan(ART_IIDS, True, "artifact")
    scan(ADOPT_IIDS, False, "adopt")
    print(f"真实数据扫 {len(ART_IIDS) + len(ADOPT_IIDS)} 条")
else:
    print("跳过真实数据（~/tmp/dataset_v22 不存在）")

if fails:
    print("FAIL:")
    for f in fails:
        print(" -", f)
    sys.exit(1)
print("dup_artifact 测试全部通过")
