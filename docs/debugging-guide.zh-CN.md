# OpenWork 同事调试指南

本文用于在一台开发机上逐项跑通 OpenWork，而不是执行完整测试矩阵。建议严格按顺序操作：先确认本机和 Docker，再启动控制面与 Employee Workspace，最后接入需要凭证的 GitHub、飞书和 AI Runtime。

## 1. 当前能验证什么

| 功能 | 本地能否直接验证 | 额外条件 |
| --- | --- | --- |
| CLI、Runtime/App 清单 | 可以 | Rust 工具链 |
| 销售分析容器演示 | 可以 | Docker，可拉取固定 BusyBox 镜像 |
| Control API、PostgreSQL、审计 | 可以 | Docker Compose |
| Employee Workspace | 可以 | Node.js 20+、正在运行的 Control API |
| Files 上传与选择 | 可以 | Control API |
| App 安装、启用、禁用 | 可以 | Control API；状态在进程重启后会清空 |
| LibreOffice 生成 XLSX/PPTX | 可以 | Docker，本地构建镜像 |
| GitHub 只读能力 | 可以 | 有限权限 GitHub Token、Docker |
| GitHub 创建 Issue | 可以 | GitHub Token、人工审批；会产生真实外部写入 |
| 飞书 MCP 工具发现 | 可以 | 飞书应用 ID/Secret、`npx` |
| 飞书业务调用 | 暂不支持 | 当前仅开放工具发现和只读白名单配置 |
| Claude/Codex/OpenCode/Pi 真实任务 | 条件式 | 已认证、digest 固定的 provider 镜像和受限网络 |

不要把“CLI 能检测到宿主机命令”当成“容器中的 AI Runtime 已可执行”。生产任务只使用显式配置的容器镜像，不直接运行宿主机上的 AI CLI。

## 2. 准备环境

需要 Git、Rust stable、Docker Desktop、Node.js 20+、npm、`curl`、`jq` 和 `openssl`。

在仓库根目录执行：

```bash
git switch main
git pull --ff-only
git status --short
rustc --version
cargo --version
docker version
node --version
npm --version
```

`git status --short` 应没有输出。Docker 必须同时显示 Client 和 Server。

创建本次调试目录和公共变量：

```bash
export OPENWORK_REPO="$PWD"
export OPENWORK_DEBUG_ROOT="${TMPDIR:-/tmp}/openwork-debug"
export OPENWORK_WORKSPACE_ROOT="$OPENWORK_DEBUG_ROOT/workspaces"
export OPENWORK_WORKER_TEMP_ROOT="$OPENWORK_DEBUG_ROOT/tmp"
export OPENWORK_WORKER_OUTPUT_ROOT="$OPENWORK_DEBUG_ROOT/outputs"
export OPENWORK_DOCKER_BIN="$(command -v docker)"
export OPENWORK_API_TOKEN="$(openssl rand -hex 32)"
mkdir -p "$OPENWORK_WORKSPACE_ROOT/demo" \
  "$OPENWORK_WORKER_TEMP_ROOT" \
  "$OPENWORK_WORKER_OUTPUT_ROOT"
```

所有路径必须是绝对路径且目录必须预先存在。不要把 Token 写入 Git、截图或聊天记录。

## 3. 五分钟基础验证

只构建调试时会用到的 CLI：

```bash
cargo build -p openwork-cli --locked
./target/debug/openwork --version
./target/debug/openwork runtime list --json | jq
./target/debug/openwork app list --registry registry/apps --json | jq
./target/debug/openwork doctor --json | jq
```

`doctor` 可以报告某个外部 Runtime 未安装或未登录；这不影响后续控制面、GitHub、Files 和 LibreOffice 调试。

运行一个不需要模型账号的真实容器演示：

```bash
OPENWORK_DOCKER_BIN="$OPENWORK_DOCKER_BIN" \
OPENWORK_DEMO_OUTPUT_ROOT="$OPENWORK_DEBUG_ROOT/sales-output" \
./scripts/demo-m1.sh
```

成功标准：命令退出码为 `0`，输出包含一个 `succeeded` run，并在 `$OPENWORK_DEBUG_ROOT/sales-output/` 下留下本次运行目录。这个演示不会发送邮件或调用真实业务系统。

## 4. 启动 Control API

默认 Compose 最适合先验证数据库和 API。它故意关闭 Worker，也不会挂载 Docker socket；因此这一步创建 AI 任务返回 `runtime_worker_unavailable` 是正确的安全行为。

为本次会话生成数据库密码，直接通过环境变量启动，避免把调试凭证落盘：

```bash
export OPENWORK_DB_PASSWORD="$(openssl rand -hex 24)"
export OPENWORK_HOST_WORKSPACE_ROOT="$OPENWORK_WORKSPACE_ROOT"
docker compose -f compose/compose.yaml up -d --build postgres openwork-control-api
docker compose -f compose/compose.yaml ps
curl --fail --silent http://127.0.0.1:8080/health | jq
```

健康响应应为 `{"status":"ok","postgres":"ok"}`。

建立后续命令使用的变量：

```bash
export OPENWORK_CONTROL_API_BASE_URL=http://127.0.0.1:8080
export OPENWORK_CONTROL_API_TOKEN="$OPENWORK_API_TOKEN"
export OPENWORK_AUTH="Authorization: Bearer $OPENWORK_API_TOKEN"
```

认证快速检查：

```bash
curl --silent --output /dev/null --write-out '%{http_code}\n' \
  http://127.0.0.1:8080/v1/apps
curl --fail --silent -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/apps | jq
```

第一条应返回 `401`，第二条应返回 GitHub、Feishu、LibreOffice 清单。查看服务日志：

```bash
docker compose -f compose/compose.yaml logs --tail=200 openwork-control-api
```

## 5. 跑通 Employee Workspace

另开一个终端，重新导出同一个 `OPENWORK_REPO`、`OPENWORK_API_TOKEN` 后执行：

```bash
cd "$OPENWORK_REPO/apps/admin-web"
npm install
OPENWORK_CONTROL_API_BASE_URL=http://127.0.0.1:8080 \
OPENWORK_CONTROL_API_TOKEN="$OPENWORK_API_TOKEN" \
npm run dev
```

按以下顺序点检：

1. Workspace 首页能显示 Control API 健康状态。
2. Files 页面上传一个小于 8 MiB、文件名只含字母数字、`.`、`_`、`-` 的文件。
3. Apps 页面能看到三个 reviewed Apps。
4. App Store 安装并启用 GitHub；禁用后再启用。
5. AI Runtimes 页面能看到 `auto`、`claude-code`、`codex`、`opencode`、`pi`。
6. Settings 将默认 Runtime 改成一个值，返回 Tasks 后新任务应采用该值。
7. Approvals 页面为空时应显示真实空状态，不应出现示例审批。

如果页面显示 `404`、`409` 或 `503`，保留页面上的服务端错误码；桌面端不会伪造成功数据。

## 6. Files API

不用桌面端也可以验证上传：

```bash
printf 'name,revenue\nAcme,120\n' > "$OPENWORK_DEBUG_ROOT/sample.csv"
curl --fail --silent \
  -H "$OPENWORK_AUTH" \
  -H 'Content-Type: application/octet-stream' \
  -H 'x-openwork-filename: sample.csv' \
  --data-binary "@$OPENWORK_DEBUG_ROOT/sample.csv" \
  http://127.0.0.1:8080/v1/files | tee "$OPENWORK_DEBUG_ROOT/upload.json" | jq
export OPENWORK_FILE_ID="$(jq -r .id "$OPENWORK_DEBUG_ROOT/upload.json")"
curl --fail --silent -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/files | jq
```

文件 ID 应由 64 位 SHA-256、连字符和原文件名组成。空文件、超过 8 MiB、路径型文件名或 `../` 必须被拒绝。

只有 Worker 启用后才能提交带文件的任务：

```bash
curl --silent --show-error \
  -H "$OPENWORK_AUTH" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg file "$OPENWORK_FILE_ID" '{runtime:"auto",workspace:"demo",prompt:"Summarize the CSV",file_ids:[$file]}')" \
  http://127.0.0.1:8080/v1/runs | jq
```

在默认 Compose 下，这里应安全返回 `503 runtime_worker_unavailable`。第 11 节说明如何启用真实 Worker。

## 7. App 管理和 CLI-Anything 导入

通过运行中的 Control API 安装 GitHub App：

```bash
OPENWORK_CONTROL_API_BASE_URL=http://127.0.0.1:8080 \
OPENWORK_CONTROL_API_TOKEN="$OPENWORK_API_TOKEN" \
./target/debug/openwork app install org.openwork.github \
  --registry registry/apps --json | jq
```

输出中的 `remote_install` 应为 `true`。随后验证启用状态：

```bash
curl --fail --silent -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/apps/org.openwork.github | jq
curl --fail --silent -X POST -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/apps/org.openwork.github/disable | jq
curl --fail --silent -X POST -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/apps/org.openwork.github/enable | jq
```

导入命令只生成待审核 YAML，不会安装或执行第三方内容：

```bash
./target/debug/openwork app import \
  --id com.example.reviewed-tool \
  --name 'Reviewed Tool' \
  --skill /absolute/path/to/SKILL.md \
  --capability reviewed.data.read \
  > "$OPENWORK_DEBUG_ROOT/reviewed-tool.yaml"
```

必须由人审核生成的 capability、风险级别、文件权限、网络和凭证声明，再放入 `registry/apps/`。不要把第三方 `SKILL.md` 当成可信权限策略。

## 8. LibreOffice：真实生成 XLSX 和 PPTX

这条路径不需要 AI 模型账号：

```bash
docker build -t openwork/libreoffice-app:dev containers/libreoffice-app
export OPENWORK_LIBREOFFICE_IMAGE="$(docker image inspect openwork/libreoffice-app:dev --format '{{.Id}}')"
OPENWORK_REAL_DOCKER_TESTS=1 \
OPENWORK_DOCKER_BIN="$OPENWORK_DOCKER_BIN" \
OPENWORK_LIBREOFFICE_IMAGE="$OPENWORK_LIBREOFFICE_IMAGE" \
cargo test -p openwork-e2e --test real_libreoffice --locked -- --ignored --nocapture
```

成功标准：测试显示 `1 passed`，内部 run 为 `succeeded`，并验证两个非空产物。这里验证的是真实 Docker、Capability Gateway、CLI provider、LibreOffice headless 和产物扫描；不是一次真实 Pi 模型调用。

## 9. GitHub MCP：读取和审批后写入

### 9.1 用宿主机方式启动 Control API

Compose 中的 Control API 没有 Docker socket，也没有注入 GitHub Token。调试 GitHub MCP 时，推荐停止 Compose 控制面，启动一个只暴露到 loopback 的 PostgreSQL，然后在宿主机运行 Control API。

```bash
docker compose -f compose/compose.yaml down
docker stop openwork-debug-postgres 2>/dev/null || true
docker run --rm -d \
  --name openwork-debug-postgres \
  -p 127.0.0.1:54329:5432 \
  -e POSTGRES_USER=openwork \
  -e POSTGRES_PASSWORD="$OPENWORK_DB_PASSWORD" \
  -e POSTGRES_DB=openwork \
  postgres:17.6-bookworm@sha256:f3bd19c606e442c3d7bdfa8002e03fe260a1023351e0ea4598032022b68dd6e3
until docker exec openwork-debug-postgres pg_isready -U openwork -d openwork >/dev/null 2>&1; do sleep 1; done
```

为 GitHub Token 只授予目标调试仓库所需的最小权限。先验证只读时不要授予写权限。

```bash
export OPENWORK_DATABASE_URL="postgresql://openwork:${OPENWORK_DB_PASSWORD}@127.0.0.1:54329/openwork"
export OPENWORK_CONTROL_BIND='127.0.0.1:8080'
export OPENWORK_API_ACTOR='debug-colleague'
export OPENWORK_APP_REGISTRY="$OPENWORK_REPO/registry/apps"
export OPENWORK_WORKER_ENABLED=0
export OPENWORK_GITHUB_TOKEN='replace-with-a-scoped-token'
export OPENWORK_GITHUB_MCP_DOCKER_BIN="$OPENWORK_DOCKER_BIN"
cargo run -p openwork-control-api
```

保持该终端运行，在另一个终端重新设置 `OPENWORK_API_TOKEN` 和 `OPENWORK_AUTH`。然后安装 App、查看 connector 和工具：

```bash
curl --fail --silent -X POST -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/apps/org.openwork.github/install | jq
curl --fail --silent -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/connectors | jq
curl --fail --silent -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/connectors/github/tools | jq
```

工具发现会启动一次固定 digest 的官方 GitHub MCP 容器；OpenWork 不接受任意 MCP 工具名。

### 9.2 真实只读调用

```bash
export GITHUB_OWNER=shichenghaoshu
export GITHUB_REPO=openwork
curl --fail --silent \
  -H "$OPENWORK_AUTH" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg owner "$GITHUB_OWNER" --arg repo "$GITHUB_REPO" \
    '{resource:("github://"+$owner+"/"+$repo),parameters:{owner:$owner,repo:$repo,state:"OPEN",perPage:5}}')" \
  http://127.0.0.1:8080/v1/apps/org.openwork.github/capabilities/github.issue.read/invoke | jq
```

成功响应的 `status` 是 `completed`。Token 不应出现在响应或服务日志中。

### 9.3 审批后创建真实 Issue

这一步有真实外部副作用。只对专用测试仓库操作，并为 Token 增加该仓库 Issues 写权限。

```bash
export ISSUE_TITLE="OpenWork debug $(date -u +%Y%m%dT%H%M%SZ)"
curl --fail --silent \
  -H "$OPENWORK_AUTH" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg owner "$GITHUB_OWNER" --arg repo "$GITHUB_REPO" --arg title "$ISSUE_TITLE" \
    '{resource:("github://"+$owner+"/"+$repo+"/issues"),parameters:{owner:$owner,repo:$repo,title:$title,body:"Created by the OpenWork governed approval debug flow."}}')" \
  http://127.0.0.1:8080/v1/apps/org.openwork.github/capabilities/github.issue.create/invoke \
  | tee "$OPENWORK_DEBUG_ROOT/approval-request.json" | jq

export APPROVAL_ID="$(jq -r .approval_id "$OPENWORK_DEBUG_ROOT/approval-request.json")"
export APPROVAL_REVISION="$(jq -r .approval_revision "$OPENWORK_DEBUG_ROOT/approval-request.json")"
export ACTION_RUN_ID="$(jq -r .run_id "$OPENWORK_DEBUG_ROOT/approval-request.json")"

curl --fail --silent \
  -X POST -H "$OPENWORK_AUTH" -H 'Content-Type: application/json' \
  -d "$(jq -n --argjson revision "$APPROVAL_REVISION" '{expected_revision:$revision,reason:"Approved for the dedicated debug repository"}')" \
  "http://127.0.0.1:8080/v1/approvals/$APPROVAL_ID/approve" \
  | tee "$OPENWORK_DEBUG_ROOT/approval-approved.json" | jq

export APPROVED_REVISION="$(jq -r .revision "$OPENWORK_DEBUG_ROOT/approval-approved.json")"
curl --fail --silent \
  -X POST -H "$OPENWORK_AUTH" -H 'Content-Type: application/json' \
  -d "$(jq -n --argjson revision "$APPROVED_REVISION" '{expected_revision:$revision}')" \
  "http://127.0.0.1:8080/v1/approvals/$APPROVAL_ID/execute" | jq

curl --fail --silent -H "$OPENWORK_AUTH" \
  "http://127.0.0.1:8080/v1/runs/$ACTION_RUN_ID" | jq
curl --fail --silent -H "$OPENWORK_AUTH" \
  "http://127.0.0.1:8080/v1/runs/$ACTION_RUN_ID/events" | jq
```

成功标准：run 为 `succeeded`，审计事件包含 approval requested/approved/consumed、action executed 和 run completed。再次调用同一个 `/execute` 必须失败，不能创建第二个 Issue。

如果远端结果不明确，OpenWork 会返回 `connector_execution_uncertain` 并拒绝自动重试；先到 GitHub 按标题确认是否已创建，再人工处理。

## 10. 飞书 MCP 工具发现

停止正在运行的 Control API，加入以下变量后重新启动：

```bash
export OPENWORK_LARK_MCP_NPX_BIN="$(command -v npx)"
export OPENWORK_LARK_APP_ID='replace-with-feishu-app-id'
export OPENWORK_LARK_APP_SECRET='replace-with-feishu-app-secret'
export OPENWORK_LARK_DOMAIN='https://open.feishu.cn'
cargo run -p openwork-control-api
```

验证：

```bash
curl --fail --silent -X POST -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/apps/org.openwork.feishu/install | jq
curl --fail --silent -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/connectors/lark/tools | jq
```

返回内容只能包含 OpenWork 固定的只读白名单。当前版本不提供飞书 capability 执行路由，因此工具发现成功后即完成本项验证，不要把它描述为已跑通飞书业务读取。

## 11. 启用真实 AI Runtime Worker

这部分只适用于已经准备好 provider 镜像的同事。镜像必须：

- 使用 registry digest（`name@sha256:...`）或本地 content ID（`sha256:...`），不能用浮动 tag；
- 包含对应 Runtime CLI；
- 在目标环境中已经解决认证，但不能把密钥烘焙进镜像层；
- 支持 OpenWork 对应的非交互 JSON/JSONL 协议；
- 能以 UID/GID `65534`、只读根文件系统运行。

先停止 Control API，创建调试网络并配置至少一个 Runtime：

```bash
docker network inspect openwork-provider-egress >/dev/null 2>&1 || \
  docker network create openwork-provider-egress
export OPENWORK_WORKER_ENABLED=1
export OPENWORK_PROVIDER_NETWORK=openwork-provider-egress
export OPENWORK_WORKER_ACTOR='worker:debug'

# 以下只填已实际构建并检查过的镜像；示例值不能直接运行。
export OPENWORK_OPENCODE_IMAGE='registry.example/opencode@sha256:replace-me'
export OPENWORK_OPENCODE_EXECUTABLE='/usr/local/bin/opencode'

cargo run -p openwork-control-api
```

上面的普通 Docker network 只用于本机联调，并不提供生产级域名/地址出口限制；生产环境必须另行实施 provider allowlist。

检查 readiness：

```bash
curl --fail --silent -H "$OPENWORK_AUTH" \
  http://127.0.0.1:8080/v1/admin/runtimes | jq
```

只有目标 Runtime 的 `configured` 和 `worker_ready` 都为 `true` 才提交任务：

```bash
curl --fail --silent \
  -H "$OPENWORK_AUTH" \
  -H 'Content-Type: application/json' \
  -d '{"runtime":"opencode","workspace":"demo","prompt":"Create a short README summary","file_ids":[]}' \
  http://127.0.0.1:8080/v1/runs \
  | tee "$OPENWORK_DEBUG_ROOT/run.json" | jq

export RUN_ID="$(jq -r .id "$OPENWORK_DEBUG_ROOT/run.json")"
while true; do
  curl --fail --silent -H "$OPENWORK_AUTH" \
    "http://127.0.0.1:8080/v1/runs/$RUN_ID" | tee "$OPENWORK_DEBUG_ROOT/run-status.json" | jq
  jq -e '.status == "succeeded" or .status == "failed" or .status == "cancelled" or .status == "timed_out"' \
    "$OPENWORK_DEBUG_ROOT/run-status.json" >/dev/null && break
  sleep 1
done

curl --fail --silent -H "$OPENWORK_AUTH" \
  "http://127.0.0.1:8080/v1/runs/$RUN_ID/events" | jq
curl --fail --silent -H "$OPENWORK_AUTH" \
  "http://127.0.0.1:8080/v1/runs/$RUN_ID/artifacts" | jq
```

取消非终态任务：

```bash
curl --fail --silent -X POST -H "$OPENWORK_AUTH" \
  "http://127.0.0.1:8080/v1/runs/$RUN_ID/cancel" | jq
```

`200` 表示已确认取消；`202` 表示 Worker 已收到 durable cancellation intent，稍后再次查询 run，直到进入 `cancelled` 或其他终态。

## 12. 最小回归检查

功能调试完成后不必先跑全仓矩阵，执行这些短门禁即可：

```bash
cargo fmt --all --check
cargo check --workspace --locked
cargo test -p openwork-control-api --locked
cargo test -p openwork-e2e --test control_plane --locked
cd apps/admin-web && npm run typecheck && npm run build
```

真实外部调用和 ignored Docker 测试不要在没有明确凭证、镜像和目标仓库时批量运行。

## 13. 常见故障

### `runtime_worker_unavailable`

默认 Compose 关闭 Worker，或者目标 Runtime 未配置。先查看 `/v1/admin/runtimes`，不要反复提交任务。

### `workspace_unavailable`

请求里的 `workspace` 必须是 `OPENWORK_WORKSPACE_ROOT` 下已经存在的相对目录，例如 `demo`，不能传绝对路径、`.` 或 `..`。

### `401 unauthorized`

Employee Workspace、CLI 和 `curl` 使用的 Token 必须与 Control API 启动时的 `OPENWORK_API_TOKEN` 完全一致，且长度至少 32 字节。

### `app_not_installed` / `app_not_enabled`

先调用 App 的 `/install`，再 `/enable`。当前 App 安装状态在 Control API 进程内，服务重启后需要重新安装/启用。

### `connector_not_configured`

GitHub 缺 `OPENWORK_GITHUB_TOKEN`，或飞书的 ID/Secret 没有成对设置。修改环境变量后必须重启 Control API。

### `connector_unavailable` / `connector_timeout`

先单独确认 Docker 或 `npx` 可执行文件为绝对路径，再检查 daemon、代理、DNS 和目标服务。不要把 Token 打印到诊断输出。

### GitHub `403`

Token 没有目标仓库的读取或 Issues 写权限。不要直接扩大到所有仓库；为专用调试仓库补最小权限。

### Runtime 很快进入 `failed`

检查：镜像是否 digest 固定、容器内 executable 是否存在、UID 65534 能否运行、输出是否符合对应 Runtime decoder、provider 网络和认证是否可用。查看 run events 和 Control API 日志，不要记录 prompt 或凭证明文。

### 服务重启后旧任务缺 prompt

当前 prompt handoff 是一次性进程内交付。服务重启后未领取的 prompt 无法恢复，Worker 会让任务安全失败；重新创建任务，不要修改数据库伪造恢复。

## 14. 清理调试环境

确认不再需要本地调试数据后执行：

```bash
docker stop openwork-debug-postgres 2>/dev/null || true
docker compose -f compose/compose.yaml down
```

Compose 的 PostgreSQL volume 默认会保留。若确实需要删除数据，应由环境负责人另行确认后再执行带 `--volumes` 的清理命令。

调试完成后请提交以下证据：

- 使用的 Git commit；
- `/health` 和 `/v1/admin/runtimes` 的脱敏输出；
- 成功 run ID、终态、artifact hash 和 audit event 类型；
- 外部写入的目标 URL；
- 哪些 Runtime/connector 没有真实凭证，因此没有验证。
