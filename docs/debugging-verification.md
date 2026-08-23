# OpenWork 调试验证记录

按 [docs/debugging-guide.zh-CN.md](debugging-guide.zh-CN.md) 在单台开发机上逐项验证，日期 2026-08-23，基线 commit `8795548`（origin/main）。

## 结论

- 第 3 节（CLI/清单/doctor/容器演示）：全部通过，`demo-m1.sh` 退出码 0，run `succeeded`。
- 第 4 节（Control API/PostgreSQL/认证）：通过；**修复了两处 Compose 部署缺陷**（见下文）。
- 第 5–7 节（Employee Workspace 数据面 / Files / App 管理）：通过；界面点检以 UI 实际调用的 API 端点验证完成（本机无显示环境，Electron 窗口无法打开）。
- 第 8 节（LibreOffice XLSX/PPTX）：通过，`real_libreoffice` 测试 1 passed。
- 第 9 节（GitHub MCP 只读 + 审批写入）：通过，真实 Issue 已创建并核验，审批流审计链完整。
- 第 10 节（飞书）：**未验证**——无飞书应用 ID/Secret；仅可见 connector 状态 `not_configured`。按指南要求，不将其描述为已跑通飞书业务读取。
- 第 11 节（真实 AI Runtime Worker）：**未验证**——无 digest 固定的 provider 镜像；Compose 模式的 `/v1/admin/runtimes` 全部 `worker_ready: false`，符合预期安全行为。

## 遇到的错误与处置（对照指南第 13 节）

指南第 13 节列出的常见故障中，实际触发并验证的只有：

| 错误 | 触发点 | 处置 |
| --- | --- | --- |
| `runtime_worker_unavailable` | 带文件提交 run；默认 Compose 无 Worker | 预期安全行为，502 语义下的 503（见下） |

其余第 13 节故障（`401`、`403`、`app_not_installed`、`connector_not_configured`、`workspace_unavailable` 等）在本轮调试中未触发。调试初期遇到的多数问题属于本机环境而非仓库缺陷：

- Docker daemon 未运行 → 启动 `systemctl start docker`；
- 缺少 docker compose 插件 → `pacman -S docker-compose`；
- 本地脚本每次生成新 API Token → 固定为文件持久化；
- Postgres 持久卷保留旧密码 → 重置调试卷；
- 调试目录属主为 root，容器内 UID 65532 无法写入 → `chown -R 65532:65532`；
- Vite 只监听 `[::1]`；Electron 无显示环境 → `ELECTRON_STARTUP_PREVENT=1`。

## 修复的仓库缺陷（本次 PR 包含）

1. **Compose 控制面缺少 App Registry 挂载**：镜像只含二进制；`load_app_registry()` 找不到目录即 fail closed，`/v1/apps` 返回空清单，与指南第 4/5 节期望（GitHub、Feishu、LibreOffice 三清单）不符。修复：`compose/compose.yaml` 增加只读挂载 `${OPENWORK_HOST_REGISTRY:-../registry/apps}` → `/registry/apps` 并设置 `OPENWORK_APP_REGISTRY=/registry/apps`。
2. **Compose 工作区只读导致 Files 上传 500**：`upload_file` 写入 `$WORKSPACE_ROOT/.openwork/uploads`，而 `/workspaces` 挂载为 `read_only` → 上传恒为 `500 internal_error`。修复：工作区挂载改为可写（Files API 持久化上传，供后续 Worker 拾取）。

## 逐节证据

### 第 3 节

- `openwork --version` → `OpenWork 0.1.0`；`runtime list`：auto/claude-code/codex 健康，opencode、pi `missing`。
- `doctor --json` → 10 PASS / 0 FAIL / 2 SKIP（OpenCode、Pi 未安装，指南注明不影响后续）。
- `scripts/demo-m1.sh` → exit 0：
  - run_id `01a02e81-530f-7ab0-b0a0-2c2a3cbe0e3e`，`status: succeeded`，`revision: 3`，`audit_event_count: 6`；
  - 产物：`sales-analysis.csv`（404 B，sha256 `b0d51ee462378286ccd4e60a951881b2be8908d6f5e21a8ae58c7bf606b745e0`）、`summary.md`（316 B，sha256 `ec643695fcd8347a3dd984e8cdf255e6d0dbb255c448b7bef55146a612ef8072`）；
  - `control_plane` e2e 2 通过。无外部邮件发送（MockActionExecutor）。

### 第 4 节

- `GET /health` → `200 {"status":"ok","postgres":"ok"}`（Compose）。
- 无 Token `GET /v1/apps` → `401`；带 Token → GitHub、Feishu、LibreOffice 三清单（修复后）。
- `/v1/admin/runtimes`（Compose）：`auto/claude-code/codex/opencode/pi` 均为 `configured: false, worker_ready: false`（Worker 关闭的预期状态）。
- 日志无 API Token 泄漏（比对日志与 Token 值，0 命中）。

### 第 5–6 节

- UI 数据面（Admin Workspace，Vite `localhost:5173` 服务正常，页面标题 OpenWork Admin）依赖的全部端点在无 UI 下逐一验证：
  - 健康状态：同 `/health`；
  - Files 上传：见下；
  - Apps：三个 reviewed Apps 均在册；App Store 安装 → 启用 → 禁用 → 再启用 GitHub 全部成功；
  - AI Runtimes：五个条目齐全；
  - Settings：`default_runtime` 由 `auto` 改为 `codex` 后 GET 确认生效；
  - Approvals：`GET /v1/approvals` → `[]`（真实空状态，无示例审批）。
- Files API：
  - 上传 `sample.csv`（22 B）→ `201`，id `7aa61061ec455e90c7aa70620e531fc77efdf2cc9657d433b5b3de1d13acb45b-sample.csv`（64 位 SHA-256 + `-` + 原文件名，符合指南）；
  - 拒绝用例：空文件 → `422 invalid_file_size`；9 MiB → `413 length limit exceeded`；`a/b.csv` → `422 invalid_filename`；`../evil.csv` → `422 invalid_filename`；
  - 用 `file_ids` 提交 run → `503 runtime_worker_unavailable`（指南第 4 节预期的安全行为，`invalid_run_request` 曾因本地脚本空 ID 误报，非代码问题）。

### 第 7 节

- `openwork app install org.openwork.github --registry registry/apps` → `remote_install: true`，经 `/v1/apps/org.openwork.github` 确认 `installed: true, enabled: true`。
- 禁用后 `enabled: false`，再启用 `enabled: true`。
- `app import` 生成待审核 YAML（`com.example.reviewed-tool`，capability `reviewed.data.read`，risk L1，网络/凭证声明齐全）；按指南未放入 `registry/apps/`（需人工审核）。

### 第 8 节

- 本地构建 `openwork/libreoffice-app:dev`；`cargo test -p openwork-e2e --test real_libreoffice -- --ignored` → `1 passed`（`real_cli_anything_libreoffice_creates_xlsx_and_pptx`，2.92s）。

### 第 9 节（真实外部写入）

- 工具发现：`/v1/connectors/github/tools` 返回 40+ 工具（含 `issue_read`、`issue_write`）。
- 只读：`github.issue.read` invoke → `status: completed`；响应与宿主日志均无 Token 值（0 命中）。
- 审批写入（目标仓库 `shichenghaoshu/openwork`）：
  - `github.issue.create` → `202 approval_required`（approval_id `01a02e92-fadd-7220-a750-198369bb8acb`，revision 0，run_id `01a02e92-facc-71a2-a093-bb87d0b67d65`）；
  - approve → `approved` revision 1；execute → `completed`，输出 GitHub issue id `5226856776`；
  - create 结果：**https://github.com/shichenghaoshu/openwork/issues/107**（标题 `OpenWork debug 20260823T122259Z`，state open，创建者 damechen163）；
  - 审计事件链：`run_created → runtime_selected → action_requested → approval_requested → approval_approved → approval_consumed → runtime_started → action_executed → run_completed`；
  - 再次 `/execute` → `409 approval_execution_rejected`（单次使用，未创建第二个 Issue）。

### 第 12 节（回归门禁）

- `cargo fmt --all --check` ✓
- `cargo check --workspace --locked` ✓
- `cargo test -p openwork-control-api --locked` → `0 tests`（结果 ok；该 crate 的测试为 feature 门控，本仓库状态下无测试装载）
- `cargo test -p openwork-e2e --test control_plane --locked` → `2 passed`
- `npm run typecheck && npm run build`（apps/admin-web）✓

### 未验证项（无凭证/镜像）

- 飞书 MCP 工具发现与业务调用：无应用 ID/Secret；指南明确当前版本不提供飞书 capability 执行路由，工具发现成功 ≠ 已跑通飞书业务读取。
- 真实 AI Runtime Worker（opencode 等）：无 digest 固定、已就绪的 provider 镜像；未配置任何 `OPENWORK_*_IMAGE`。
