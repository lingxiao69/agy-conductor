# 参与贡献指南 (Contributing to AgyConductor)

非常欢迎任何形式的开源贡献！无论是提交 Bug 反馈、体验优化建议、跨 CLI 驱动适配，还是跨平台支持，我们都非常期待你的加入。

## 1. 本地开发环境准备

1. **操作系统**：Windows 10 / 11（内置 Microsoft Edge WebView2 Runtime）；跨平台适配（macOS / Linux）正在规划中，欢迎认领。
2. **工具链**：
   - Rust 稳定版（`1.78+`，Windows 下推荐 `x86_64-pc-windows-msvc`）
   - Node.js（可选，仅用于本地运行 `node --check` 校验前端原生 ES Module 语法）
   - 本机已安装 `agy`（Antigravity CLI）用于真机联调；仅运行单元测试/集成测试无需真实模型配额。

## 2. 常用开发与验证命令

```powershell
# 运行全量单元测试与集成测试（含模拟 CLI 流式输出、调度器、计费与会话健康度测试）
cargo test

# 校验前端原生 JS 模块语法
Get-ChildItem assets/js/*.js | ForEach-Object { node --check $_.FullName }

# 开发模式启动桌面界面
cargo run

# 编译 Release 单文件可执行程序
cargo build --release --bin agy-conductor

# 命令行环境与模型探测冒烟检查
.\target\release\agy-conductor.exe --probe
```

## 3. 代码结构与工程规范

为了让代码库长期保持清爽易读、对人类开发者和 AI 编码助手都极度友好，提交 PR 时请遵循以下约定：

- **高内聚模块化拆分**：每个源码、样式、脚本或测试文件请保持**严格少于 500 物理行**（`< 500` 行）。当模块接近上限时，请按职责边界拆分子模块，不要通过压缩空行或删减必要注释来凑行数。
- **文件编码**：所有文本源码统一使用 **UTF-8 无 BOM**（`UTF-8 without BOM`）。
- **零外部前端重框架**：前端界面位于 `assets/`，采用原生 ES Modules + 模块化 CSS tokens 构建，保持 ~4.5 MB 原生体积与毫秒级冷启动体验。
- **隐私与只读底线**：
  - 严禁在代码、测试或提交记录中硬编码任何个人路径、API Key、Token 或内部域名；
  - 对用户本机的 CLI 配置文件（如 `~/.gemini/antigravity-cli/settings.json`、`~/.claude`、`~/.codex` 等）必须保持**严格只读**，运行时所需的合并配置请生成在应用专属数据目录中。
- **测试覆盖**：修复调度状态机、流式解析器、计费统计或平台守护相关问题时，建议在 `tests/` 补充对应的回归测试用例。

## 4. 提交 Pull Request 流程

1. Fork 本仓库并从 `main` 创建特性分支（例如 `feat/codex-runner` 或 `fix/session-resume`）。
2. 完成开发并确保 `cargo test` 零警告、零失败通过。
3. 提交清晰的中文或英文 Commit 说明，发起 Pull Request 并简述改动背景与验证方式。
