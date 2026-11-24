---
name: feature-development
description: Implement Haven features with PRD alignment, scoped changes, tests, and reporting. Use when asked to build or modify a feature, including 开发功能 and 实现需求. Do not use for security review or test-only acceptance.
---

# Feature Development

按顺序执行，不要跳步。每一步完成后才进入下一步。

## 1. 读取约束

- 读 `AGENTS.md`；改动安全敏感文件时同时读 `.trae/rules/security.md`。
- 在 `docs/Haven-PRD-产品需求分析-v0.2.md` 中定位与任务相关的 FR / SEC / DP 条目。
- 确认任务属于当前阶段；PRD 未覆盖的取舍先列出并说明，不得自行扩大需求。
- 只引用需要的需求编号，不要把整份 PRD 复制进代码、注释或测试。

## 2. 理解代码

- 前端：读相关 `src/components/`、`src/views/`、`src/lib/`。
- 后端：读 `src-tauri/src/lib.rs` 的命令注册，以及 `vault.rs`、`session.rs` 的相关函数。
- 搜索现有测试，确认可复用的测试工具、命名与断言风格。
- 标出与任务无关的模块，保持不动。

## 3. 明确边界

用 3 到 6 条列出：

- 本次要做的内容。
- 明确不做的内容。
- 是否触碰加密、密钥、文件落盘或 Tauri 权限。

优先切出一个可运行、可验证的垂直切片，不要同时铺开多个未完成模块。

## 4. 简短方案

- 说明要改的文件、数据流、错误码与状态变化。
- 涉及加密或密钥时，先按 `.trae/rules/security.md` 自检再动手。
- 重要设计变更写明理由、影响范围和回滚方式。

## 5. 实现

- 前端只负责界面与交互；文件系统、加密、密钥必须留在 Rust。
- 不写模拟数据、假接口，也不制造“看起来可用”的未实现功能。
- 不新增依赖，除非说明必要性并得到确认。
- 不在日志、错误提示或注释输出密码、密钥、文件路径或文件内容。
- 未实现的入口必须显式标注，不得默认隐藏。

## 6. 编写测试

- Rust：在对应模块加 `#[cfg(test)]` 测试，覆盖成功、失败与边界。
- 前端纯函数：加到 `tests/*.test.mjs`。
- 安全相关改动至少覆盖一个失败场景（错误密码、篡改、缺失、越界等）。

## 7. 运行测试

根据改动范围执行：

```bash
cd src-tauri && cargo fmt --check
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo test
npm test
npm run build
```

涉及界面或桌面集成时，再执行 `npm run tauri:dev` 实际启动验证。
失败时先定位根因，不得通过放宽断言掩盖缺陷。

## 8. 汇报

按以下结构汇报：

1. 修改与创建的文件。
2. 执行过的命令。
3. 检查与测试结果（失败项保留原文）。
4. 遗留问题与下一步建议。
