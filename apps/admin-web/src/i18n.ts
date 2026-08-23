// 双语界面文案（默认中文，可在界面右上角切换英文）。状态机映射随当前语言输出。
// 服务器返回的错误码本身是英文，因此英文模式下直接透传。

export type Locale = 'zh' | 'en'

const messages: Record<Locale, Record<string, string>> = {
  en: {
    'nav.workspace': 'Workspace',
    'nav.tasks': 'Tasks',
    'nav.files': 'Files',
    'nav.apps': 'Apps',
    'nav.approvals': 'Approvals',
    'nav.connectors': 'Connectors',
    'app.workspace': 'Employee Workspace',
    'app.workspaceHint': 'Create a task, add the files it needs, and stay informed when a decision needs you.',
    'app.admin': 'Administration',
    'app.adminShort': 'Runtimes · App Store · Settings',
    'app.connected': 'Control API connected',
    'app.offline': 'Control API offline',
    'app.refresh': 'Refresh',
    'app.hubHint': 'Your tasks, files, apps, and decisions live here.',
    'app.newTask': 'New task',
    'app.welcomeTitle': 'Start work. Keep context close.',
    'tasks.eyebrow': 'Controlled execution',
    'tasks.title': 'Tasks',
    'tasks.runtime': 'Runtime',
    'tasks.workspace': 'Workspace ID',
    'tasks.workspacePlaceholder': 'team/project',
    'tasks.prompt': 'Task prompt',
    'tasks.promptPlaceholder': 'Describe the task for the controlled worker.',
    'tasks.files': 'Files to use',
    'tasks.noFiles': 'No files are available.',
    'tasks.working': 'Working…',
    'tasks.create': 'Create task',
    'tasks.detail': 'Task detail',
    'tasks.appsUsed': 'Apps used',
    'tasks.artifacts': 'Artifacts',
    'tasks.actions': 'Actions',
    'tasks.approvals': 'Approvals',
    'tasks.updated': 'Updated',
    'tasks.reason': 'Reason',
    'tasks.cancel': 'Cancel task',
    'tasks.noDetail': 'Create a task to follow its details here.',
    'tasks.audit': 'Audit events',
    'tasks.notReported': 'Not reported',
    'files.eyebrow': 'Shared task context',
    'files.title': 'Files',
    'files.uploading': 'Uploading…',
    'files.upload': 'Upload file',
    'files.empty': 'No files returned by the Control API.',
    'files.details': 'File details',
    'files.type': 'Type',
    'files.size': 'Size',
    'files.added': 'Added',
    'files.fileId': 'File ID',
    'files.hint': 'Choose this file from New task to include it in the work context.',
    'files.selectHint': 'Select a file to view its details.',
    'files.sizeUnavailable': 'Size unavailable',
    'files.typeUnavailable': 'Unavailable',
    'files.timeUnavailable': 'Unavailable',
    'apps.eyebrow': 'Available for your work',
    'apps.title': 'Apps',
    'apps.empty': 'No apps returned by the Control API.',
    'apps.available': 'Available',
    'apps.unavailable': 'Unavailable',
    'apps.noDescription': 'No description returned.',
    'apps.capabilities': 'Capabilities',
    'apps.risk': 'Risk level',
    'apps.resource': 'Resource',
    'apps.parameters': 'Parameters (JSON)',
    'apps.invoke': 'Invoke',
    'apps.invoking': 'Invoking…',
    'apps.invokeCompleted': 'Invocation completed',
    'apps.invokeApprovalRequired': 'Approval required: open Approvals, approve, then execute.',
    'apps.gotoApprovals': 'Open approvals',
    'apps.approvalId': 'Approval ID',
    'apps.expiresAt': 'Expires at',
    'apps.invokeResult': 'Invocation result',
    'apps.githubCreateTitle': 'Title',
    'apps.githubCreateBody': 'Body',
    'apps.githubCreateOwner': 'Repository owner',
    'apps.githubCreateRepo': 'Repository name',
    'apps.githubCreateRepoHint': 'Owner and repository are also submitted as the resource (e.g. github://owner/repo/issues).',
    'apps.selectCapability': 'Select a capability to invoke it here.',
    'apps.paramRequired': ' (required)',
    'apps.noParams': 'This capability takes no parameters; a default plan is used.',
    'approvals.eyebrow': 'Human decision point',
    'approvals.title': 'Approvals',
    'approvals.empty': 'No approval requests returned by the Control API.',
    'approvals.run': 'Run',
    'approvals.requestedBy': 'requested by',
    'approvals.expires': 'Expires',
    'approvals.approve': 'Approve',
    'approvals.deny': 'Deny',
    'approvals.execute': 'Execute',
    'approvals.executing': 'Executing…',
    'connectors.eyebrow': 'Available integrations',
    'connectors.title': 'Connectors',
    'connectors.empty': 'No connectors returned. If this Control API does not yet provide /v1/connectors, its real server error is shown above.',
    'connectors.tools': 'Tools',
    'connectors.noToolsFor': 'No tools returned for {name}.',
    'connectors.selectHint': 'Select a connector to query its tools.',
    'connectors.status.notConfigured': 'Not configured',
    'connectors.status.unknown': 'Unknown',
    'admin.eyebrow': 'Administrator',
    'admin.runtimes': 'AI Runtimes',
    'admin.store': 'App Store',
    'admin.settings': 'Settings',
    'admin.noRuntimes': 'No runtimes returned by the Control API.',
    'admin.noStore': 'No app store entries returned by the Control API.',
    'admin.enabled': 'Enabled',
    'admin.disabled': 'Disabled',
    'admin.notInstalled': 'Not installed',
    'admin.saving': 'Saving…',
    'admin.install': 'Install',
    'admin.enable': 'Enable',
    'admin.disable': 'Disable',
    'admin.defaultRuntime': 'Default runtime',
    'admin.saveRuntime': 'Save runtime preference',
    'admin.credentialNote': 'Provider credentials remain in the Control API secret boundary and are never sent to this desktop renderer.',
    'admin.noSettings': 'No settings returned by the Control API.',
    'demo.salesTitle': 'Sales analysis container demo',
    'demo.runSales': 'Run demo',
    'demo.runningSales': 'Demo running… (requires Docker)',
    'demo.artifacts': 'Artifacts',
    'demo.auditEvents': 'Audit events',
    'demo.outputDirectory': 'Output directory',
    'common.unreported': 'Not reported',
    'common.unknown': 'Unknown',
  },
  zh: {
    'nav.workspace': '工作区',
    'nav.tasks': '任务',
    'nav.files': '文件',
    'nav.apps': '应用',
    'nav.approvals': '审批',
    'nav.connectors': '连接器',
    'app.workspace': '员工工作区',
    'app.workspaceHint': '把信息放在手边。创建任务、挂上需要的文件，需要你决策时第一时间知情。',
    'app.admin': '管理员',
    'app.adminShort': '运行时 · 应用商店 · 设置',
    'app.connected': 'Control API 已连接',
    'app.offline': 'Control API 未连接',
    'app.refresh': '刷新',
    'app.hubHint': '你的任务、文件、应用与决策都在这里。',
    'app.newTask': '新建任务',
    'app.welcomeTitle': '开始工作，上下文尽在掌握。',
    'tasks.eyebrow': '受控执行',
    'tasks.title': '任务',
    'tasks.runtime': '运行时',
    'tasks.workspace': '工作区 ID',
    'tasks.workspacePlaceholder': '团队/项目',
    'tasks.prompt': '任务提示词',
    'tasks.promptPlaceholder': '向受控 Worker 描述这个任务。',
    'tasks.files': '要使用的文件',
    'tasks.noFiles': '暂无可用文件。',
    'tasks.working': '处理中…',
    'tasks.create': '创建任务',
    'tasks.detail': '任务详情',
    'tasks.appsUsed': '使用的应用',
    'tasks.artifacts': '产物',
    'tasks.actions': '动作',
    'tasks.approvals': '审批',
    'tasks.updated': '更新时间',
    'tasks.reason': '结束原因',
    'tasks.cancel': '取消任务',
    'tasks.noDetail': '创建任务后，详情会显示在这里。',
    'tasks.audit': '审计事件',
    'tasks.notReported': '未报告',
    'files.eyebrow': '共享任务上下文',
    'files.title': '文件',
    'files.uploading': '上传中…',
    'files.upload': '上传文件',
    'files.empty': 'Control API 未返回任何文件。',
    'files.details': '文件详情',
    'files.type': '类型',
    'files.size': '大小',
    'files.added': '上传时间',
    'files.fileId': '文件 ID',
    'files.hint': '在“新建任务”中选择该文件，即可把它加入任务上下文。',
    'files.selectHint': '选择文件以查看详情。',
    'files.sizeUnavailable': '大小不可用',
    'files.typeUnavailable': '不可用',
    'files.timeUnavailable': '不可用',
    'apps.eyebrow': '可为你使用',
    'apps.title': '应用',
    'apps.empty': 'Control API 未返回任何应用。',
    'apps.available': '可用',
    'apps.unavailable': '不可用',
    'apps.noDescription': '未返回描述。',
    'apps.capabilities': '能力',
    'apps.risk': '风险等级',
    'apps.resource': '资源',
    'apps.parameters': '参数（JSON）',
    'apps.invoke': '调用',
    'apps.invoking': '调用中…',
    'apps.invokeCompleted': '调用完成',
    'apps.invokeApprovalRequired': '需要审批：请前往审批页批准，然后执行。',
    'apps.gotoApprovals': '前往审批处理',
    'apps.approvalId': '审批编号',
    'apps.expiresAt': '过期时间',
    'apps.invokeResult': '调用结果',
    'apps.githubCreateTitle': '标题',
    'apps.githubCreateBody': '正文',
    'apps.githubCreateOwner': '仓库所有者',
    'apps.githubCreateRepo': '仓库名',
    'apps.githubCreateRepoHint': '所有者与仓库名随资源（resource）一并提交，例如 github://owner/repo/issues。',
    'apps.selectCapability': '选择能力后在此调用。',
    'apps.paramRequired': '（必填）',
    'apps.noParams': '该能力无参数，将使用默认计划。',
    'approvals.eyebrow': '人工决策点',
    'approvals.title': '审批',
    'approvals.empty': 'Control API 未返回任何审批请求。',
    'approvals.run': '运行',
    'approvals.requestedBy': '请求者',
    'approvals.expires': '过期时间',
    'approvals.approve': '批准',
    'approvals.deny': '拒绝',
    'approvals.execute': '执行',
    'approvals.executing': '执行中…',
    'connectors.eyebrow': '可用集成',
    'connectors.title': '连接器',
    'connectors.empty': '未返回连接器。如果这个 Control API 尚不支持 /v1/connectors，其真实错误显示在上方。',
    'connectors.tools': '工具',
    'connectors.noToolsFor': '未返回 {name} 的工具。',
    'connectors.selectHint': '选择连接器以查询其工具。',
    'connectors.status.notConfigured': '未配置',
    'connectors.status.unknown': '未知',
    'admin.eyebrow': '管理员',
    'admin.runtimes': 'AI 运行时',
    'admin.store': '应用商店',
    'admin.settings': '设置',
    'admin.noRuntimes': 'Control API 未返回任何运行时。',
    'admin.noStore': 'Control API 未返回应用商店条目。',
    'admin.enabled': '已启用',
    'admin.disabled': '已禁用',
    'admin.notInstalled': '未安装',
    'admin.saving': '保存中…',
    'admin.install': '安装',
    'admin.enable': '启用',
    'admin.disable': '禁用',
    'admin.defaultRuntime': '默认运行时',
    'admin.saveRuntime': '保存运行时偏好',
    'admin.credentialNote': '提供方凭据保持在 Control API 的密钥边界内，不会发送到此桌面渲染进程。',
    'admin.noSettings': 'Control API 未返回任何设置。',
    'demo.salesTitle': '销售分析容器演示',
    'demo.runSales': '运行演示',
    'demo.runningSales': '演示运行中…（需要 Docker）',
    'demo.artifacts': '产物',
    'demo.auditEvents': '审计事件',
    'demo.outputDirectory': '输出目录',
    'common.unreported': '未报告',
    'common.unknown': '未知',
  },
}

let locale: Locale = (() => {
  try {
    const stored = localStorage.getItem('openwork.locale')
    return stored === 'en' ? 'en' : 'zh'
  } catch {
    return 'zh'
  }
})()

export function getLocale(): Locale {
  return locale
}

export function setLocale(next: Locale): void {
  locale = next
  try { localStorage.setItem('openwork.locale', next) } catch { /* ignore storage errors */ }
}

/** 取当前语言的界面文案；vars 用于 {placeholder} 插值。 */
export function t(key: string, vars?: Record<string, string | number>): string {
  const value = messages[locale][key] ?? messages.en[key] ?? key
  if (!vars) return value
  return Object.entries(vars).reduce(
    (acc, [name, replacement]) => acc.replaceAll(`{${name}}`, String(replacement)),
    value,
  )
}

const runStatus: Record<Locale, Record<string, string>> = {
  en: {
    queued: 'Queued', planning: 'Planning', awaiting_approval: 'Awaiting approval',
    running: 'Running', succeeded: 'Succeeded', failed: 'Failed',
    cancelled: 'Cancelled', timed_out: 'Timed out',
  },
  zh: {
    queued: '排队中', planning: '规划中', awaiting_approval: '等待审批',
    running: '运行中', succeeded: '成功', failed: '失败',
    cancelled: '已取消', timed_out: '已超时',
  },
}

/** Run 状态 → 当前语言。 */
export function runStatusText(status?: string): string {
  if (!status) return t('common.unreported')
  return runStatus[locale][status] ?? status
}

const runtimeStatus: Record<Locale, Record<string, string>> = {
  en: { ready: 'Ready', configured: 'Configured', not_configured: 'Not configured' },
  zh: { ready: '就绪', configured: '已配置', not_configured: '未配置' },
}

/** Runtime 状态 → 当前语言。 */
export function runtimeStatusText(status?: string): string {
  if (!status) return t('common.unknown')
  return runtimeStatus[locale][status] ?? t('common.unknown')
}

const approvalStatus: Record<Locale, Record<string, string>> = {
  en: { pending: 'Pending', approved: 'Approved', denied: 'Denied', expired: 'Expired', consumed: 'Consumed' },
  zh: { pending: '待审批', approved: '已批准', denied: '已拒绝', expired: '已过期', consumed: '已使用' },
}

/** 审批状态 → 当前语言。 */
export function approvalStatusText(status?: string): string {
  if (!status) return t('common.unknown')
  return approvalStatus[locale][status] ?? status
}

/** 时间戳 → 当前语言本地化显示。 */
export function formatTime(value?: string): string {
  if (!value) return '—'
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) return value
  return parsed.toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US')
}

const serverError: Record<string, string> = {
  demo_not_configured: '演示未配置：控制面缺少 OPENWORK_LIBREOFFICE_IMAGE。',
  demo_sandbox_unavailable: '演示沙箱不可用：请确认 Docker 守护进程正在运行。',
  demo_unavailable: '演示不可用：请确认 Docker 守护进程与演示镜像可用。',
  runtime_worker_unavailable: '运行时不可用：Worker 未启用或目标 Runtime 未配置（见调试指南 §13）。',
  worker_unavailable: '运行时不可用：Worker 未就绪。',
  connector_not_configured: '连接器未配置：缺少对应的凭证环境变量。',
  connector_unavailable: '连接器不可用：请检查 Docker/HTTP 服务是否可达。',
  capability_not_mapped: '能力未映射：此能力没有受支持的 MCP 工具对。',
  capability_dispatch_unavailable: '该能力暂不支持调用（当前仅开放固定的能力派发）。',
  capability_action_not_executable: '该动作不可执行：当前只允许 GitHub Issue 创建。',
  invalid_capability_parameters: '能力参数无效：请检查参数 JSON 是否符合能力输入格式。',
  invalid_run_request: '任务请求无效：请检查运行时、工作区、提示词与文件。',
  invalid_workspace_id: '工作区无效：必须是工作区根目录下的相对目录名（例如 demo）。',
  approval_execution_rejected: '审批执行被拒绝：审批已使用或版本不匹配。',
  app_not_installed: '应用未安装：请先在应用商店安装。',
  app_not_enabled: '应用未启用：请先启用该应用。',
}

/** 后端错误码 → 中文；英文模式下（或未收录错误）透传原文。 */
export function serverErrorText(error: Error | string): string {
  const message = error instanceof Error ? error.message : String(error)
  return locale === 'zh' ? serverError[message] ?? message : message
}

/** 连接器状态小标签 → 当前语言。 */
export function connectorStatusText(status?: string): string {
  if (!status) return t('connectors.status.unknown')
  if (status === 'not_configured') return t('connectors.status.notConfigured')
  if (status === 'ready' || status === 'online') return locale === 'zh' ? '就绪' : 'Ready'
  if (status === 'error' || status === 'down') return locale === 'zh' ? '离线' : 'Offline'
  return status
}
