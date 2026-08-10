# OpenWork

面向中小企业的开源 AI 工作环境安装系统。

一家公司安装一次，全体员工即可获得统一的私有 AI 助手，并由身份、权限、
策略和审批决定它能读取什么、调用什么以及执行什么。

[English](README.md) · [快速开始](docs/getting-started.md) ·
[为客户部署](docs/deploy-for-client.md) · [开发 Capability Pack](docs/packs/build-your-first-pack.md)

> 当前状态：`v0.1-bootstrap`。原生 Rust CLI 已支持版本、结构化 doctor/status、
> runtime 查询命令和无副作用安装计划。

## 员工未来可以完成

- 在授权范围内查询企业知识；
- 在隔离 Sandbox 内分析表格、生成文档；
- 使用只读凭证分析允许访问的业务数据；
- 仅在策略允许或审批通过时调用业务工具。

## 为 AI 实施商设计

- 一套部署服务一家企业；
- 通过版本化 Capability Pack 与 Adapter 扩展，不侵入核心；
- 以统一方式诊断、备份、升级、回滚和售后。

Community 核心采用 Apache-2.0，允许提供商业实施服务，但必须同时遵守第三方
组件许可证。详见[许可证说明](docs/licensing.md)。

## Bootstrap 开发验证

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
./target/release/openwork --version
./target/release/openwork status --json
./target/release/openwork doctor --json
./target/release/openwork install --dry-run --json
```

五个 Tier 1 原生构建目标的发布包由 [POSIX](scripts/install.sh) 与
[PowerShell](scripts/install.ps1) 脚本安装；脚本会先
校验 SHA-256，默认拒绝覆盖现有二进制，只有显式 force 才会先备份再替换。参见
[发布检查清单](docs/release/checklist.md)与可复现的
[Bootstrap 演示](docs/demo/bootstrap-runtime.md)。

平台支持证据见 [platform-support.md](docs/platform-support.md)，fixture、CI 烟测和真机验证不会混为一谈。
