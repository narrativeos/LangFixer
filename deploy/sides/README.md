# sides:决策模型(:4804)+ 字级判别侧车(:4811)

## 宿主 venv 模式(首选)

宿主需 Python ≥3.10(torch/transformers 要求)。Alibaba Cloud Linux 3 / RHEL8 系:

    dnf -y install python3.11 python3.11-pip        # 不动系统 /usr/bin/python3(3.6,dnf 依赖)

venv 两个服务依赖相同,建一个、链一个:

    python3.11 -m venv /srv/langfixer/decision-model/.venv
    .venv/bin/pip install -i https://mirrors.aliyun.com/pypi/simple/ fastapi uvicorn transformers
    .venv/bin/pip install torch --index-url https://download.pytorch.org/whl/cpu
    ln -sfn ../decision-model/.venv /srv/langfixer/feature-model/.venv_char

之后 `cp deploy/sides/langfixer-{decision,char-gate}.service /etc/systemd/system/`、
`systemctl daemon-reload && systemctl enable --now langfixer-decision langfixer-char-gate`。
venv 路径与 api-server supervisor 的 `bring_up_local`(decision-model/.venv、
feature-model/.venv_char)一致;systemd 常驻时 supervisor 探到已健康即跳过拉起。

- 决策:默认绑 `127.0.0.1:4804`(`DEC_HOST`/`DEC_PORT` 可改),不对外。
- 侧车:`char_gate.py --host`/`CHAR_GATE_HOST` 控 bind,默认 127.0.0.1。
- **L3 在 docker bridge 里的机器**(如网关):侧车要同时给宿主 api 和 L3 容器看见,
  绑定 docker bridge 网关 IP 而非 127.0.0.1——unit 里加 `--host 172.18.0.1`,
  `langfixer.env` 加 `GEC_CHAR_GATE_ENDPOINT=http://172.18.0.1:4811`(api 探测口),
  `langfixer-l3.service` 加 `-e L3_CHAR_GATE_URL=http://172.18.0.1:4811/gate`。
  三者只改其一会错位:面板探测走 api 的 endpoint,L3 挂载走自己的 env。
- 绑 0.0.0.0 = 公网裸奔,禁止;只允许 127.0.0.1 或 bridge 网关 IP。

## 老 Python 宿主回退:容器模式

宿主装不了新 Python(如仅剩 3.6)时用共享镜像跑代码挂载:

    docker build -t langfixer-sides:1 deploy/sides/Dockerfile

镜像只装 fastapi/uvicorn/CPU torch/transformers,代码与权重 volume 挂载;
容器内绑 0.0.0.0、宿主仅 `-p 127.0.0.1:4804:4804`(侧车同理 4811,L3 场景再加一条
`-p <bridge-GW>:4811:4811`),unit 形态见 git 历史 `4d73014`。
升级 Python 后应切回 venv 模式(启动快、代码热改只需 restart)。
