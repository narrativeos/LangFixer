# sides:决策模型(:4804)+ 字级判别侧车(:4811)

宿主 Python 过旧(网关 3.6)时,用本目录镜像以容器方式跑这两个本机依赖:

    docker build -t langfixer-sides:1 deploy/sides/
    cp deploy/sides/langfixer-{decision,char-gate}.service /etc/systemd/system/
    systemctl daemon-reload && systemctl enable --now langfixer-decision langfixer-char-gate

- 代码与权重 volume 挂载(`decision-model:/app:ro`、`feature-model:/app:ro`),镜像只装
  fastapi/uvicorn/torch(cpu)/transformers;`server.py`/`char_gate.py` 更新后仅需 restart。
- 决策:容器内 `DEC_HOST=0.0.0.0`,宿主仅发布 `127.0.0.1:4804`(不对外)。
  权重在 `decision-model/models/<lang>/`(encoder/ + head.pt + meta.json;trackb 双头)。
- 侧车:`CHAR_GATE_HOST` 控制 bind(`char_gate.py --host`),容器内 0.0.0.0、宿主仅 127.0.0.1:4811。
- L3 在 docker bridge 里的机器(如网关):char-gate 单元需**额外**发布一条
  `-p <docker-bridge-GW>:4811:4811`(网关上是 `172.18.0.1`),并在 langfixer-l3.service
  加 `-e L3_CHAR_GATE_URL=http://<bridge-GW>:4811/gate`;否则 L3 以启发式回退运行
  (字级侧车在检测状态条仍显示可用,因为 api-server 探的是 127.0.0.1:4811)。
- 与 api-server supervisor 的 `bring_up_local`(本机 venv 直拉)二选一;systemd 常驻优先,
  supervisor 见 4804/4811 已健康即跳过拉起。
