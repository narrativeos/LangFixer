# 决策模型（:4804）与字级判别侧车（:4811）共用的宿主侧镜像。
# 网关宿主 Python 过旧（3.6），统一用 python:3.12 + CPU torch 跑；
# 代码与权重全部 volume 挂载（decision-model/ 或 feature-model/），镜像只装依赖。
# 构建：docker build -t langfixer-sides:1 deploy/sides/
FROM python:3.12-slim
RUN pip install --no-cache-dir \
      -i https://mirrors.aliyun.com/pypi/simple/ \
      fastapi uvicorn transformers \
 && pip install --no-cache-dir torch \
      --index-url https://download.pytorch.org/whl/cpu
