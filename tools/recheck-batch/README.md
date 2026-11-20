# 批量复查（recheck-batch）

`python3 tools/recheck-batch/batch_recheck.py` —— 把 detect_history 里决策模型
「句级判断」留下的灰区句批量送 `POST /gec/recheck`（③-T 定向复查闭环，复查结论
自动落 feedback_log 的 `recheck_found/recheck_clean`），凑 P3 灰区决策表要的
**≥20 条 feedback_recheck 样本**（方案 B，2026-11-20 定案）。

前置：api-server 在跑且 LLM 已配置（否则复查路由 503，脚本明确报错退出）。
每句一次 LLM 调用。

## 用法

```
python3 tools/recheck-batch/batch_recheck.py --dry-run    # 只列候选
python3 tools/recheck-batch/batch_recheck.py              # 默认凑到 20 条
```

已复查过的句子（feedback_log 有 recheck_* 记录）与本轮重复句自动跳过；
按 detect_history id 倒序取最新灰区。

## 首跑数字（2026-11-20 dry-run）

现有样本 6 条（found 1 : clean 5），需补 14；候选去重后 26 句——存量够凑满。
