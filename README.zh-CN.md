# OpenWork

开源的企业 AI Agent 执行控制平面。

一家公司安装一次，全体员工即可获得统一的私有 AI 助手，并由身份、权限、
策略和审批决定它能读取什么、调用什么以及执行什么。

[English](README.md) · [快速开始](docs/getting-started.md) ·
[为客户部署](docs/deploy-for-client.md) · [开发 Capability Pack](docs/packs/build-your-first-pack.md)

> 当前状态：M1 完成工作正在集成。真实容器销售演示、Postgres 控制状态、
> 策略/审批/动作控制、产物与哈希链审计已实现；持久化 worker 租约与 fail-closed
> 取消意图已实现，通用 worker 执行循环及安全 prompt 交付仍缺失。证据分级见
> [当前状态](CURRENT_STATE.md)。

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

## 开发验证

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
./scripts/demo-m1.sh
./target/release/openwork --version
./target/release/openwork status --json
./target/release/openwork doctor --json
./target/release/openwork install --dry-run --json
```

五个 Tier 1 原生构建目标的发布包由 [POSIX](scripts/install.sh) 与
[PowerShell](scripts/install.ps1) 脚本安装；脚本会先
校验 SHA-256，默认拒绝覆盖现有二进制，只有显式 force 才会先备份再替换。参见
[发布检查清单](docs/release/checklist.md)与可复现的
[Bootstrap 演示](docs/demo/bootstrap-runtime.md)。完整交付范围与已知限制见
[alpha 发布说明](docs/release/v0.1.0-alpha.1.md)。新的 M1 源码流程见
[快速开始](docs/getting-started.md)；在 M1 集成合并并发布之前，已有发布包仍是
Bootstrap alpha。

平台支持证据见 [platform-support.md](docs/platform-support.md)，fixture、CI 烟测和真机验证不会混为一谈。
