# Kev-9B 中文门控微调——3090 租卡 Runbook

试点设计见 EVOLUTION.md §11.5/§11.6。语料 = `gate_ft/`（train 9,866 / dev 1,000，
与冻结测试集 `gate_eval/zh_gate_1k.jsonl` 文本级零重叠，构建脚本 `gate_ft_build.py`）。
管线已在 M2 Max MPS 上用 0.8B smoke 全链路验证（train→serve→gate_eval）。

## 0. 预算与预期

- 卡：RTX 3090 24GB（AutoDL 约 ¥1.5–2/h）
- 训练：9B LoRA（`--weights_dtype bf16` ≈18GB 权重 + 短句激活 ≈2–3GB，24GB 放得下）
- 预计 1.5–2.5 s/rec × 19,732 rec（9,866×2 epochs）≈ **10–14 h ≈ ¥20–30**
- 验收线（默认启用门槛，§11.6）：zh AUC ≥0.85；漏检≤2% 时跳过率 ≥20%；
  校准后 ECE <0.05；门控延迟可接受

## 1. 开机与装环境（租 3090，选 PyTorch 2.6+/CUDA 12 镜像）

```bash
pip install uv
git clone --depth 1 https://github.com/jaredpalmer/kev kev
cd kev && uv sync --extra serve
# 关键加速（MPS 上没有、3090 上有）：
uv pip install flash-linear-attention causal-conv1d
```

## 2. 搬运数据（从 Mac）

```bash
scp LangFixer/decision-model/gate_ft/{train,dev}.jsonl <GPU>:/root/kev/gate_ft/
scp LangFixer/decision-model/gate_eval.py <GPU>:/root/kev/
scp LangFixer/decision-model/gate_eval/zh_gate_1k.jsonl <GPU>:/root/kev/gate_eval/
```

## 3. 训练

```bash
cd /root/kev
HF_ENDPOINT=https://hf-mirror.com uv run python -m kev.train \
  --data gate_ft/train.jsonl \
  --base Qwen/Qwen3.5-9B-Base --init_from jaredpalmer/kev-9b \
  --epochs 2 --lr 2e-5 --batch 1 --accum 8 \
  --dtype bf16 --weights_dtype bf16 --checkpointing 1 \
  --device cuda --seed 20261019 --out runs/gate-zh-9b
```

要点：
- `--init_from` 是 warm start（官方对照：从 base 训 0.33 vs init_from 0.83），必须带；
- 国内网络下载 base 权重走 hf-mirror 很快（18GB 约 20–40 分钟）；
- OOM 兜底：`--lora 8`，或改 `--base Qwen/Qwen3.5-4B-Base --init_from jaredpalmer/kev-4b`；
- 别在同机跑第二个训练任务。

## 4. 训练侧体检（dev 切片）

```bash
uv run python -m kev.benchmark --run runs/gate-zh-9b --data gate_ft/dev.jsonl --out runs/gate-zh-9b-eval
```

看 accuracy / Brier / 校准。注意：**这只是 dev，验收必须回到 gate_eval 冻结集**。

## 5. 起服务 + 回 Mac 跑冻结验收（唯一有效数字）

GPU 机上：

```bash
uv run --extra serve python -m kev.serve --run runs/gate-zh-9b --port 8009
```

Mac 上开隧道并跑 gate_eval（996 句全量）：

```bash
ssh -L 8009:localhost:8009 <GPU>
cd LangFixer/decision-model
.venv/bin/python gate_eval.py run --arm kev4b --endpoint http://127.0.0.1:8009 \
  --model gate-zh-9b --out gate_eval/results_kev9b_ft.jsonl
.venv/bin/python gate_eval.py score --results gate_eval/results_kev9b_ft.jsonl --arm kev4b
```

对照基线（同集）：kev9b 零样本 AUC 0.765 / kev4b 0.751 / laya ≈0.5。

## 6. 回传与本地常驻（验收通过后）

微调产物只有 adapter + head（几百 MB），回传后本机 `kev.serve` 走 MPS 推理
（9B 推理 32GB+ Mac 可跑，官方预期；延迟需实测）。是否进产品按 §11.6 验收线判定。

## 7. 停机

租卡实例记得关机/释放；`runs/gate-zh-9b` 先 scp 回本地再释放实例。

## 8. 双 3090：并行跑两个实验（不是多卡训练一个模型）

代码事实（kev/train.py:527）：LoRA 模式 `world` 恒为 1，**不支持 DDP**；
torchrun+FSDP2 仅服务 `--full_ft 1`（9B 全参微调，10k 句上过拟合/漂移风险大，
且与 `--init_from` 的 warm start 语义不符——不用）。

正确用法：一张卡一个进程，互不干扰（`CUDA_VISIBLE_DEVICES` 隔离；
「一次只跑一个任务」的限制是 MPS 的，NVIDIA 分卡不受限）：

```bash
# GPU0：主实验 9B（§3 命令）
CUDA_VISIBLE_DEVICES=0 HF_ENDPOINT=https://hf-mirror.com uv run python -m kev.train \
  --data gate_ft/train.jsonl --base Qwen/Qwen3.5-9B-Base --init_from jaredpalmer/kev-9b \
  --epochs 2 --lr 2e-5 --batch 1 --accum 8 --dtype bf16 --weights_dtype bf16 \
  --checkpointing 1 --device cuda --seed 20261019 --out runs/gate-zh-9b

# GPU1：对照实验 4B（产品更优候选：门控延迟 265ms vs 367ms、内存减半）
CUDA_VISIBLE_DEVICES=1 HF_ENDPOINT=https://hf-mirror.com uv run python -m kev.train \
  --data gate_ft/train.jsonl --base Qwen/Qwen3.5-4B-Base --init_from jaredpalmer/kev-4b \
  --epochs 2 --lr 2e-5 --batch 1 --accum 8 --dtype bf16 --weights_dtype bf16 \
  --checkpointing 1 --device cuda --seed 20261019 --out runs/gate-zh-4b
```

4B 预计 5–7h 先完成，可提前起 serve 跑一轮冻结验收。两个都完事后，
同一把尺（gate_eval 冻结集 + §0 验收线）择优；若 4B 过线优先选 4B
（门控路径要的是又快又准，不是又大）。备选：GPU1 改跑 9B 超参变体
（如 `--lr 5e-5` 或 `--lora 32`），做同尺择优。

## 9. 双 Radeon AI PRO R9700（32GB, RDNA4/ROCm）

代码事实：`kev/device.py` 明确支持 CUDA/ROCm（ROCm 版 torch 暴露 `torch.cuda`
API，`--device cuda` 照常工作）；`fused_qwen35.py`（fla/Triton）仅用于 serving，
**训练走纯 PyTorch 参考实现 + PEFT**，无 NVIDIA 专属依赖——MPS smoke 跑通的
就是同一条参考路径，所以 AMD 上能跑，但没有 fla 加速。

硬前提与预算：
- **bf16 必须可用**（RDNA4 有 bf16 硬件；ROCm autocast 需验证）：
  9B 的 fp32 权重 36GB > 32GB，bf16 失败就只能退回 4B；
- 显存本身富余：9B bf16 权重 ≈18GB + 激活 ≈3GB；
- 速度预计 15–25h/run（ROCm 利用率打折 + 参考 kernel），仍是双卡
  并行 9B + 4B 两个实验的排法（§8 命令不变，`CUDA_VISIBLE_DEVICES`
  在 ROCm 下同样生效，写作 `HIP_VISIBLE_DEVICES` 亦可）。

开机第一件事——栈冒烟（10 分钟内出结论，别先下载 18GB 权重）：

```bash
# 1) ROCm 与 gfx 架构在位？
rocminfo | grep -i gfx12          # R9700 应为 gfx1200/1201
# 2) PyTorch ROCm wheel 是否支持这张卡（支持矩阵随版本变，实测为准）
uv pip install torch --index-url https://download.pytorch.org/whl/rocm6.4
python -c "import torch; x=torch.randn(1024,1024,device='cuda',dtype=torch.bfloat16); print((x@x).sum(), torch.cuda.get_device_name())"
# 3) 过 → 跑 kev 0.8B smoke（§3 命令换 --base Qwen/Qwen3.5-0.8B-Base
#    --init_from jaredpalmer/kev-0.8b --max_steps 20），测 s/rec 外推总时长
```

冒烟任何一步失败（matmul 崩 / bf16 报错 / 设备不可见）= 该 ROCm×torch×gfx120X
组合未转正，别硬磕：用 §8 的 3090 路径，AMD 卡留作训后验证机。


