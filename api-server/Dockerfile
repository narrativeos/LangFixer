# 构建上下文为仓库根目录（`docker build -f api-server/Dockerfile .`）；
# 配合根目录 .dockerignore 排除 target / node_modules 等
FROM rust:1.93-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p api-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/api-server /usr/local/bin/api-server
# 数据文件：决策表 + 规则词表 + LLM 提示词模板（均以绝对路径注入，避免容器 CWD 相对路径缺失）
# 注意：知识库（knowledge-base/）按 IP 边界策略刻意不 COPY 进镜像（.dockerignore 已排除）——
# 仓库内置的 src-sample-en 为自撰示例源（可分发），但用户导入的书籍属用户数据，
# 镜像不分发任何知识库内容；需要知识源富化的部署环境在运行时挂载/注入知识库目录并设置 GEC_KNOWLEDGE_DIR
COPY decision-tables /app/decision-tables
COPY lexicons /app/lexicons
COPY prompts /app/prompts
ENV GEC_TABLES_DIR=/app/decision-tables
ENV GEC_LEXICONS_DIR=/app/lexicons
ENV LLM_PROMPTS_DIR=/app/prompts
ENV HANLP_ENDPOINT=http://127.0.0.1:8000
EXPOSE 4801
CMD ["api-server"]
