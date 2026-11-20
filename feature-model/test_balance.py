"""balance.balance_confirmed 单测：python3 test_balance.py 或 pytest 均可跑。"""

from balance import balance_confirmed


def test_error全保留_clean按5倍封顶地板兜底():
    rows = [{"label": 1, "text": f"e{i}"} for i in range(2)] + [
        {"label": 0, "text": f"c{i}"} for i in range(100)
    ]
    kept, info = balance_confirmed(rows)
    # cap = max(5×error, FLOOR) = max(10, 20) = 20
    assert info["clean_cap"] == 20
    assert info["error_kept"] == 2 and info["clean_kept"] == 20 and info["clean_in"] == 100
    assert len(kept) == 22
    assert sum(1 for r in kept if r["label"] == 1) == 2


def test_未超封顶原样保留():
    rows = [{"label": 1}] + [{"label": 0}] * 9
    kept, info = balance_confirmed(rows)
    assert len(kept) == 10 and info["clean_kept"] == 9 and info["clean_in"] == 9


def test_无error用FLOOR保底且等距覆盖():
    rows = [{"label": 0, "text": str(i)} for i in range(60)]
    kept, info = balance_confirmed(rows)
    assert info["clean_cap"] == 20 and info["clean_kept"] == 20
    # 等距抽样必须确定性覆盖首、尾与中段
    texts = [r["text"] for r in kept]
    assert texts[0] == "0" and texts[-1] == "59"
    assert any(25 <= int(t) <= 35 for t in texts)  # 中段必有样本


def test_缺label按error不静默丢():
    kept, info = balance_confirmed([{}])
    assert info["error_kept"] == 1 and len(kept) == 1


def test_空输入():
    kept, info = balance_confirmed([])
    assert kept == [] and info["clean_in"] == 0


if __name__ == "__main__":
    import sys

    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for fn in fns:
        fn()
    print(f"{len(fns)} balance tests passed")
    sys.exit(0)
