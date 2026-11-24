---
name: test-and-acceptance
description: Verify a Haven feature against its requirements, add missing tests, and report pass, fail, and unverified items. Use when asked for 测试验收 or to verify a finished feature. Do not use for implementing features.
---

# Test and Acceptance

## 1. 读取需求与验收标准

- 从 `docs/Haven-PRD-产品需求分析-v0.2.md` 找到对应的 FR / SEC / DP 条目与验收口径。
- 读 `AGENTS.md`；涉及安全敏感代码时同时读 `.trae/rules/security.md`。
- 把验收标准拆成可判定的检查项：输入、预期结果、失败表现。

## 2. 检查现有测试

- Rust：查看对应模块的 `#[cfg(test)]` 与集成测试。
- 前端：查看 `tests/*.test.mjs`。
- 列出已覆盖项、只覆盖成功路径的项、完全缺失的项。
- 不重复已有测试；优先补真正缺失的失败场景与边界。

## 3. 补充必要测试

- 每个新增测试只验证一个明确行为。
- 覆盖：成功、失败、边界、异常输入、状态不允许时的操作。
- 安全相关必须包含失败场景，例如错误密码、篡改元数据、越界参数、锁定后访问。
- 测试不得输出密码、密钥或真实敏感文件内容。

## 4. 执行测试

按范围执行并保留原始输出：

```bash
cd src-tauri && cargo fmt --check
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo test
npm test
npm run build
```

必要时执行 `npm run tauri:dev` 做人工验收，逐项确认界面状态与提示文案。
不得把“编译通过”当作“功能通过”。

## 5. 检查失败场景与边界

- 逐个确认失败路径的提示是否可读，是否泄露路径或内部细节。
- 确认失败后不会留下半成品、幽灵条目或可继续使用的旧状态。
- 确认未实现的功能在界面上有明确标注，而不是看起来可用。
- 无法在自动化中验证的项，标记为“未验证”，并说明原因。

## 6. 输出验收结论

必须分三组列出，不得合并：

- 通过项：对应需求编号、验证方式与结果。
- 失败项：复现步骤、实际结果、期望结果、相关文件位置。
- 未验证项：为什么无法验证，需要什么条件。

最后给出是否可以进入下一阶段的明确判断；存在未解决的失败项时不得判定通过。
