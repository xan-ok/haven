# Haven

[English](README.en.md) · **简体中文**

离线优先的个人文件加密与隐私管理桌面应用。数据保存在本机，不上传、不同步、无需联网。

> **当前正式发布版本：Haven v0.1.3**，支持 **macOS Apple Silicon（arm64）**。
> 当前安装包**未使用 Developer ID 签名，也未经过 Apple 公证**，Gatekeeper 可能拒绝运行，详见[安装说明](#下载与安装)。

- 产品官网：https://37lur66wyyzfjjex5tg.traesite.cn/
- GitHub Releases：https://github.com/xan-ok/haven/releases/tag/v0.1.3
- Git Tag：`v0.1.3`
- 产品需求文档：[docs/Haven-PRD-产品需求分析-v0.2.md](docs/Haven-PRD-产品需求分析-v0.2.md)
- 0.1.3 发布说明：[docs/RELEASE-NOTES-0.1.3.md](docs/RELEASE-NOTES-0.1.3.md)
- 变更记录：[CHANGELOG.md](CHANGELOG.md)
- 开发约束：[AGENTS.md](AGENTS.md)

## 当前实现范围

已实现：

- 创建保险库、主密码规则、解锁、手动锁定、5 分钟无操作自动锁定
- 真实文件导入（单文件/多选/文件夹递归/拖拽），分块加密写入
- 加密索引、文件名搜索、列表与网格视图、图片缩略图
- 收藏、文件夹（新建/重命名/删除/移动文件）
- 文件重命名、删除、查看详情、完整性校验
- 图片（JPG/PNG）、PDF、视频（MP4/MOV/M4V/WebM）内存解密预览
- 单文件、批量、文件夹导出，并给出未加密副本警告
- 保险库备份与从备份恢复
- 主题中心：6 套内置主题即时切换，支持导入 / 预览 / 应用 / 删除 `.haventheme` 自定义主题，损坏自动回退

未实现或不支持：

- Developer ID 签名与 Apple 公证（当前构建环境缺少证书与 notarytool 凭据）
- Intel / Universal 架构、Windows、Linux
- 网络卷上的保险库
- 主密码修改与找回、回收站、云同步

## 后续平台支持待办

以下是**尚未开发**的平台支持计划，仅作待办记录，不代表已开始开发，也没有承诺时间。

| 平台 | 状态 | 说明 |
|---|---|---|
| macOS Apple Silicon（arm64） | ✅ 已支持 | v0.1.3 正式发布，当前唯一可用平台 |
| macOS Intel（x86_64） / Universal | ⏳ 待开发支持 | 需在 Intel 机器或交叉编译环境验证 |
| Windows（x86_64） | ⏳ 待开发支持 | 需实现 Windows 的路径、权限与安装包流程 |
| Linux（x86_64） | ⏳ 待开发支持 | 需确定发行版与打包格式 |
| iOS | ⏳ 待开发支持 | 移动端需重新设计交互、文件访问与备份模型 |
| Android | ⏳ 待开发支持 | 移动端需重新设计交互、文件访问与备份模型 |

macOS Apple Silicon 之外的平台目前**均不可下载、不可运行**。后续若开始支持，会先更新本清单和发布说明，再提供对应安装包。

## 下载与安装

### 系统要求

- macOS Apple Silicon（M1 / M2 / M3 / M4），macOS 13 Ventura 及以上
- Intel Mac、Windows、Linux 当前不支持

### 下载

**Haven v0.1.3** 正式发布：

| 项目 | 值 |
|---|---|
| 文件名 | `Haven_0.1.3_aarch64.dmg` |
| 架构 | arm64 |
| 大小 | 6.11 MiB |
| SHA-256 | `d7e0e15873b9091253b1eeb5276f88a001b525fd324a97f205af932e65f3ba6f` |
| Git Tag | `v0.1.3` |
| 构建源码提交 | `c4a8e3aa4435d8a8483d73896bd2341c22a30238` |
| 构建时间 | 2026-11-25T15:34:52 |
| 下载地址 | https://github.com/xan-ok/haven/releases/tag/v0.1.3 |

### 完整性校验

下载后在终端运行（macOS 自带 `shasum`）：

```bash
shasum -a 256 Haven_0.1.3_aarch64.dmg
```

输出必须与上方 SHA-256 完全一致。

### 安装步骤

1. 从 GitHub Releases 下载 `Haven_0.1.3_aarch64.dmg`，校验 SHA-256。
2. 打开 DMG，将 `Haven.app` 拖入「应用程序」。
3. 当前包**未签名、未公证**。首次打开时系统会提示「无法验证开发者」。在访达中右键点击 `Haven.app` → 选择「打开」→ 确认打开。

**不要关闭系统完整性保护（SIP）或全局 Gatekeeper**。仅为 Haven 这一个应用放行即可。

### 安装包状态（诚实说明）

- **签名**：未使用 Developer ID Application 签名（构建机未安装 Apple Developer 证书）。
- **公证**：未经过 Apple 公证（构建机未配置 notarytool 凭据）。
- **Gatekeeper**：首次打开会被拒绝，用户必须手动允许。

---

## 基本使用

### 创建保险库

1. 启动 Haven，点击「创建保险库」。
2. 点击「选择位置」，选择一个父目录；Haven 会在其中创建固定名称的「我的保险库」。
3. 输入至少 12 个字符的主密码并再次确认，勾选「我已了解：忘记主密码将无法恢复任何文件」。
4. 点击「创建保险库」，完成后直接进入已解锁状态。

### 导入文件

- 点击「导入文件」选择一个或多个文件，或点击「导入文件夹」递归导入其中的普通文件；也支持把文件拖入窗口。
- 导入是逐块加密写入，原文件保留在原位置，不会自动删除。
- 导入结果会显示成功与失败数量。

### 搜索与整理

- 顶部搜索框按文件名筛选。
- 可在「全部文件 / 收藏」等分类间切换，支持列表与网格视图。
- 支持新建文件夹、移动文件、重命名、删除与查看详情。

### 预览

- 点击文件行或缩略图打开预览，按 `Esc` 关闭。
- 图片支持 JPG/PNG（单文件上限 20 MB），PDF 上限 20 MB，视频支持 MP4/MOV/M4V/WebM（上限 64 MB）。
- 预览内容在内存中解密，不写入临时明文文件；超出限制的文件会提示不支持预览。

### 导出

- 单文件导出：在文件行点击「导出」。
- 批量导出：勾选多个文件后点击「批量导出」；文件夹也支持导出。
- **导出的文件是未加密明文副本，离开 Haven 后不再受加密保护**，界面会显示相应警告，请自行妥善保管。

### 备份与恢复

- 「备份保险库」会把当前保险库的加密文件复制到你选择的目录，备份保持加密状态。
- 「恢复保险库」或锁定页的「从备份恢复」：选择备份目录 → 输入创建该备份时的主密码完成验证 → 选择恢复位置，Haven 会在其中创建新的「我的保险库」。
- 恢复不会覆盖或替换原有保险库。

### 主题切换

- 已解锁界面点击顶部「外观」按钮打开主题中心。
- 6 套内置主题即时切换（Haven Original / Snow / Midnight / Forest / Sand / Lavender）。
- 支持导入 `.haventheme` 自定义主题，导入后先预览再应用；主题文件格式由 Rust 侧校验，拒绝未知字段和脚本。
- 主题偏好独立于保险库存放，损坏时自动回退 Haven Original。

### 锁定与退出

- 点击「锁定保险库」立即清理后端主密钥并回到锁定页。
- 连续 5 分钟无界面操作会自动锁定；导入、导出、预览等长任务不会暂停计时。
- 关闭窗口即退出应用；再次启动时需要重新输入主密码。

---

## 安全边界与已知限制

- 主密钥只存在于 Rust 后端内存中，锁定或退出时使用 `zeroize` 清零；前端只拿到状态、路径和错误信息。
- 忘记主密码将无法解密保险库内任何内容，Haven 不提供找回或重置。
- **普通文件删除不等于安全擦除**：删除只移除保险库内的加密数据，不承诺覆写磁盘，也不承诺清除文件系统、系统备份或存储设备层面的残留。
- **不承诺清除操作系统缓存、预览内容或其他系统级明文痕迹**：包括交换空间、休眠镜像、崩溃转储、系统日志、Spotlight/缩略图缓存等。
- 当前**不支持将保险库放在网络卷**（SMB/AFP/NFS 等），请使用本机磁盘或本地外置磁盘。
- 外置磁盘拔出后，Haven **不会实时监听拔盘**；下一次保险库操作才会触发挂载检查与失败关闭，并把会话置为不可用，需要重新连接磁盘并重新输入主密码。
- 休眠或系统锁屏**不代表 Haven 会立即自动锁定**；自动锁定只由应用内的 5 分钟无操作计时触发。
- 不提供错误次数锁定；离线暴力破解的主要防线是 Argon2id 的计算成本。

---

## 技术栈

| 层 | 技术 | 版本 |
| --- | --- | --- |
| 桌面壳 | Tauri | 2.12.7（CLI 2.12.6） |
| 前端框架 | Vue | 3.5 |
| 前端语言 | JavaScript | — |
| UI 组件库 | Naive UI | 2.45 |
| 构建工具 | Vite | 8.3 |
| 后端 | Rust | 1.91（edition 2024） |
| 加密 | Argon2id / XChaCha20-Poly1305 | argon2 0.6 / chacha20poly1305 0.11 |

## 环境要求

- macOS（当前仅支持 macOS Apple Silicon，需 Xcode Command Line Tools）
- Node.js 20 及以上
- Rust 稳定版工具链（`rustup` 安装）

## 开发

```bash
npm install
npm run tauri:dev     # 启动桌面应用
npm run dev           # 仅前端（浏览器预览，无本地保险库能力）
```

## 构建

```bash
npm run build         # 前端产物
npm run tauri:build   # macOS .app / .dmg，输出在 src-tauri/target/release/bundle/
```

## 检查与测试

```bash
npm run build
npm test                                        # 前端纯函数测试（158 项）
cd src-tauri && cargo check                     # Rust 编译检查
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo fmt --check
cd src-tauri && cargo test                      # Rust 单元测试（265 项）
```

## 加密与元数据

创建保险库时：

1. 由操作系统安全随机数生成 256-bit 主密钥与 16 字节盐值。
2. 使用 Argon2id 从主密码派生 32 字节密钥。
3. 使用 XChaCha20-Poly1305 加密主密钥，认证附加数据（AAD）覆盖全部未加密元数据字段。
4. 文件按块使用 XChaCha20-Poly1305 加密写入，加密索引记录文件元数据与内容摘要。

实际采用的参数：

| 项目 | 值 |
| --- | --- |
| KDF | Argon2id（v0x13） |
| 内存 | 262144 KiB（256 MiB） |
| 迭代 | 5 |
| 并行度 | 1 |
| 盐值 | 16 字节，每个保险库独立随机 |
| AEAD | XChaCha20-Poly1305 |

位置记忆保存在应用配置目录（macOS 通常为 `~/Library/Application Support/app.haven.desktop/vault-location.json`），其中只有保险库目录路径，不包含密码或密钥。

---

## 目录结构

```
haven/
├── docs/                         产品与发布文档
├── haven-website/                静态产品官网（已部署）
├── src/                          前端源码（Vue 3 SFC，JavaScript）
│   ├── components/               创建、解锁、文件面板、预览与详情组件
│   ├── lib/                      文件列表、选择、导出、预览、拖拽等前端控制器
│   └── views/HomeView.vue        四状态界面与自动锁定
├── tests/                        前端纯函数测试（node --test）
├── src-tauri/                    桌面端（Rust）
│   ├── src/vault.rs              密码派生、元数据加解密与原子写入
│   ├── src/session.rs            会话状态、主密钥生命周期、挂载检查
│   ├── src/file_store.rs         分块加密文件、加密索引、导入、完整性校验与预览
│   ├── src/backup.rs             保险库备份
│   ├── src/restore.rs            从备份恢复与密码验证
│   ├── src/export_txn.rs         导出事务与暂存
│   ├── src/export_staging.rs     导出暂存目录与卷一致性检查
│   ├── src/theme_store.rs        主题中心存储与校验
│   └── capabilities/             权限配置（最小权限）
├── CHANGELOG.md                  变更记录
├── AGENTS.md                     开发约束
└── README.md
```

---

## 版本历史

| 版本 | Git Tag | 说明 |
|---|---|---|
| v0.1.3 | `v0.1.3` | **当前正式发布**。主题中心：6 套内置主题，支持导入自定义 `.haventheme`，主题与保险库完全隔离，损坏自动回退。 |
| v0.1.2 | `v0.1.2` | 内部测试版。调整已解锁界面布局：文件列表占满剩余窗口高度，顶部工具栏紧凑化。 |
| v0.1.1 | `v0.1.1` | 内部测试版。修复「保险库不可用」页无法创建新保险库的问题。 |
| v0.1.0 | `v0.1.0` | 内部测试版。核心保险库功能：创建、导入、加密、预览、备份、恢复全部交付。 |
