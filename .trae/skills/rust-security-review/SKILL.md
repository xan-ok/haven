---
name: rust-security-review
description: Review Haven Rust code for key lifecycle, crypto, randomness, error leakage, metadata validation, and memory hygiene. Use when asked for 安全审查 or before merging vault and session changes. Do not use for implementation.
---

# Rust Security Review

这是一个只读审查流程。不得修改业务代码；需要修复时先给出建议，由用户或开发流程执行。

## 1. 确定审查范围

- 明确要审查的文件或改动：通常是 `src-tauri/src/vault.rs`、`src-tauri/src/session.rs`、`src-tauri/src/lib.rs`、`src-tauri/capabilities/`、`tauri.conf.json`。
- 直接读取源代码，不要只依赖提交说明或开发者的解释。
- 检查相关测试，确认失败场景是否真的被覆盖。

## 2. 按清单审查

### 密码与密钥生命周期

- 主密钥是否只在 Rust 侧，是否可能通过命令返回值、事件或序列化结构泄露到前端。
- 锁定、退出、错误路径是否都清理密钥；新增结构是否正确使用 `Zeroizing`。
- 密码是否被 trim、归一化或静默修改；长度是否按 Unicode 码点计算。
- 是否有密码、密钥进入日志、错误信息、配置或测试输出。

### 加密与随机数

- 是否自行发明算法、KDF 流程或文件格式。
- AEAD nonce 是否唯一，是否由 CSPRNG 生成。
- AAD 是否覆盖全部未加密元数据；新增字段是否同步更新。
- 盐值、密钥、nonce 是否来自操作系统 CSPRNG，而非时间或计数器。

### 错误处理

- 对外错误是否泄露路径、文件名或内部状态。
- 是否存在处理外部输入时的 `unwrap` / `expect` 或可触发 panic 的路径。
- 认证失败是否与数据损坏区分过度，是否泄露可被利用的信息。

### 元数据与资源消耗

- 是否校验版本、算法、字段长度、base64 解码结果。
- KDF 参数是否设上限，恶意元数据能否造成内存或 CPU 耗尽。
- 缺失或损坏的元数据是否会触发自动重建或覆盖。

### 内存与明文残留

- 密码、派生密钥、明文缓冲区是否在使用后清零。
- 临时文件是否在失败路径被清理；是否会留下有效半成品。
- 是否存在不必要的明文落盘、缓存或调试输出。

### 权限与配置

- `capabilities/` 是否最小权限；新增权限是否有明确理由。
- CSP 是否被放宽；是否引入了网络请求能力。

### 测试

- 是否覆盖错误密码、篡改、缺失、参数越界、锁定后访问、失败不产生半成品。
- 测试是否只验证成功路径；失败场景缺失应视为风险。
- 不得因为测试通过就认定实现安全。

## 3. 输出结论

每条问题必须包含：

- 文件路径与具体函数或行号。
- 风险描述：攻击者能做什么，或在什么条件下会泄露。
- 严重程度：高 / 中 / 低。
- 修复建议：具体到 API 或数据结构，不要只写“加强校验”。

无法确认的问题标注“需要验证”，不要猜测为结论。最后单独列出已检查但未发现问题的项，避免只汇报坏消息。

## 4. 参考命令

```bash
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo test
```

命令只作为辅助证据，不能替代源码阅读。
