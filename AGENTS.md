# AGENTS.md

本文件记录 Haven 项目的基本开发约束，供后续开发者与 AI 助手共同遵守。

## 项目概述

Haven 是一款离线优先的个人文件加密与隐私管理桌面应用，面向 macOS 个人用户。

- 技术栈：Tauri 2 + Rust + Vue 3（JavaScript）+ Naive UI + Vite
- 产品需求：`docs/Haven-PRD-产品需求分析-v0.2.md`（需求以该文档为准）
- 当前阶段：第四阶段，已完成保险库创建、主密码派生、解锁与锁定、真实文件导入、分块加密存储、加密索引、完整性校验，以及文件列表与 JPG/PNG 内存解密预览；**尚未实现 PDF 预览、导出、删除**

## 项目结构

```
src/                 前端源码（Vue 3 SFC，JavaScript）
  App.vue            根组件，负责 Naive UI 主题与语言
  main.js            入口
  theme.js           主题覆盖
  styles.css         全局样式与视觉变量
  lib/               前端工具（密码规则、Tauri 命令封装）
  components/        创建、解锁、已解锁面板
  views/             页面级组件
tests/               前端纯函数测试（node --test）
src-tauri/           桌面端（Rust）
  src/main.rs        程序入口
  src/lib.rs         Tauri 命令与应用启动逻辑
  src/vault.rs       密码派生、AEAD、元数据校验与原子写入
  src/session.rs     会话状态、主密钥生命周期、位置配置、导入与锁定协调
  src/file_store.rs  分块加密文件格式、加密索引、导入事务、完整性校验与图片预览解密
  capabilities/      权限配置（最小权限原则）
  tauri.conf.json    窗口、构建、打包配置
docs/                产品与设计文档
```

前后端职责：

- 前端只负责界面呈现与交互，不实现加密、文件读写、密钥管理。
- 涉及文件系统、加密、密钥的敏感能力放在 Rust 侧，通过明确的命令接口暴露。
- 新增前端依赖前先确认是否必要；不引入云端 SDK、遥测、在线 AI 相关依赖。

## 开发命令

```bash
npm install               # 安装前端依赖
npm run dev               # 仅前端（http://localhost:1420）
npm run tauri:dev         # 启动桌面应用
npm run build             # 前端构建
npm run tauri:build       # 打包 macOS 应用
cd src-tauri && cargo check    # Rust 编译检查
cd src-tauri && cargo clippy -- -D warnings
cd src-tauri && cargo fmt --check
```

## 项目 Rules

- 产品需求以 `docs/Haven-PRD-产品需求分析-v0.2.md` 为准；PRD 未覆盖的取舍，先说明再实现。
- 修改代码前先理解现有项目结构。
- 不随意重构与当前任务无关的代码。
- 优先完成当前任务，不擅自扩大需求。
- 优先交付可运行、可验证的垂直功能切片，再考虑扩展。
- 不允许用模拟数据或假功能冒充真实实现。
- 每次修改后执行相关检查与测试。
- 遇到依赖、编译或运行问题，先定位根因再修复。
- 不允许自行发明加密算法、密钥派生流程或加密文件格式；必须使用经过审查的成熟库与参数。
- 涉及密码、密钥、加密和文件安全的代码，必须进行额外审查。
- 不得在日志中输出密码、密钥或敏感文件内容。
- 重要设计变更必须说明理由、影响范围和回滚方式。
- 完成任务后汇报修改文件、执行的命令、测试结果及遗留问题。

## 产品底线（影响代码决策）

- 离线优先：应用运行期间不得发起任何外部网络请求（含遥测、崩溃上报、更新检查、CDN 资源）。
- 不引入云端服务、账号体系、远程同步。
- 不得在日志、错误提示、崩溃信息中输出文件名、文件路径、密码、密钥或文件内容。
- 不得把普通文件删除描述为"安全擦除"；不得承诺清除操作系统层面的明文残留（见 PRD 第 9 章）。
- 界面上不得出现未实现却表现为可用的功能；未实现状态必须显式标注。

## 安全相关代码的额外要求

涉及以下内容时，必须单独审查并说明理由，不得默认沿用示例代码：

- 密码输入、密码校验、密码派生
- 密钥的生成、存储、使用与销毁
- 文件加密写入、解密读取
- 临时文件、缓存、日志的落盘内容
- Tauri 权限配置（`capabilities/`）与 CSP

## 汇报格式

完成任务后按以下结构汇报：

1. 修改与创建的文件
2. 执行过的命令
3. 检查与测试结果（含失败项原文）
4. 遗留问题与下一步建议

## AI 协作入口

- `AGENTS.md`（本文件）：始终生效的项目级规则。
- `.trae/rules/security.md`：安全敏感文件的专项规则，按 globs 匹配 `src-tauri` 下的安全代码。
- `.trae/skills/feature-development/SKILL.md`：新功能开发流程。
- `.trae/skills/rust-security-review/SKILL.md`：Rust 安全审查流程（只读）。
- `.trae/skills/test-and-acceptance/SKILL.md`：测试与验收流程。

需要 Developer / Security Reviewer 角色时，使用子代理（`general-purpose` 或 `Explore`）并让它先加载对应 Skill；安全审查必须只读，不得直接修改开发成果。
