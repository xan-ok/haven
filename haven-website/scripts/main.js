/* Haven — static site runtime
 * i18n, scroll reveal, FAQ, lightbox, mobile nav.
 */
(function () {
  "use strict";

  /* ========== i18n ========== */

  var LOCALE_KEY = "haven-site-locale";
  var SUPPORTED = ["zh", "en"];
  var DEFAULT = "zh";

  var I18N = {
    zh: {
      "nav.features": "功能",
      "nav.security": "安全",
      "nav.open": "开源",
      "nav.download": "下载",
      "nav.cta": "GitHub",
      "nav.lang": "EN",

      "hero.eyebrow": "本地优先 · 离线 · 端到端",
      "hero.title": "你的文件，由你<em>掌控</em>。",
      "hero.lead": "Haven 是一款离线优先的个人文件加密与隐私管理工具。文件留在本地，加密由你掌控，无需云端，也不依赖网络。",
      "hero.cta.primary": "在 GitHub 上查看",
      "hero.cta.secondary": "了解核心功能",
      "hero.meta.local": "数据保存在本机",
      "hero.meta.offline": "无需联网使用",
      "hero.meta.oss": "源代码公开",

      "philosophy.eyebrow": "为什么需要 Haven",
      "philosophy.title": "你的文件，值得有一个安静的地方。",
      "philosophy.lead": "今天，我们的照片、文档和私人数据，散落在各种云盘、聊天软件、邮箱和 App 里。Haven 给你一个<strong>由你自己控制的本地加密空间</strong>——不上传、不同步、不需要登录、不需要网络。",
      "philosophy.p1.title": "不依赖云端",
      "philosophy.p1.body": "没有服务器，没有 API，没有账号体系。你就是自己的云。",
      "philosophy.p2.title": "不要求注册",
      "philosophy.p2.body": "启动即使用，邮箱、手机、姓名一概不需要。",
      "philosophy.p3.title": "不需要联网",
      "philosophy.p3.body": "导入、加密、预览、备份，全部离线完成。",
      "philosophy.p4.title": "不会自动上传",
      "philosophy.p4.body": "原文件保留在原位置，保险库不会把数据送去任何地方。",
      "philosophy.p5.title": "不会自动同步",
      "philosophy.p5.body": "不会把文件悄悄复制到另一台设备。",

      "features.eyebrow": "核心功能",
      "features.title": "一个安静、可靠的私人抽屉。",
      "features.lead": "Haven 做的事情不多，但每一件都认真完成。下面是当前版本实际实现的内容。",

      "f1.title": "Private Vault · 个人保险库",
      "f1.body": "创建独立的加密保险库，用只有你知道的主密码守护它。保险库数据放在你选择的目录里。",
      "f1.l1": "本地创建独立保险库",
      "f1.l2": "主密码保护，Argon2id 派生密钥",
      "f1.l3": "XChaCha20-Poly1305 认证加密",

      "f2.title": "File Import · 文件导入",
      "f2.body": "把你已经散落在各处的文件，慢慢收进自己的保险库。",
      "f2.l1": "单文件、多文件、文件夹递归导入",
      "f2.l2": "拖拽导入",
      "f2.l3": "分块加密写入，原文件保留原位置",

      "f3.title": "Organize · 整理",
      "f3.body": "保险库里的文件，你知道在哪里、是什么。",
      "f3.l1": "文件列表与网格视图",
      "f3.l2": "文件名搜索、收藏、文件夹",
      "f3.l3": "重命名、移动、删除、查看详情",

      "f4.title": "Preview · 预览",
      "f4.body": "Haven 会在<strong>内存里解密</strong>文件，你可以查看，它不会在磁盘上留下明文副本。",
      "f4.l1": "JPG / PNG 图片预览",
      "f4.l2": "PDF 预览",
      "f4.l3": "MP4 / MOV / WebM 视频预览",

      "f5.title": "Backup & Restore · 备份与恢复",
      "f5.body": "把整个加密保险库完整备份到另一块磁盘，或者恢复到新位置。",
      "f5.l1": "备份保持加密状态",
      "f5.l2": "恢复时需要主密码验证",
      "f5.l3": "恢复到新位置，不覆盖原保险库",

      "f6.title": "Theme Center · 主题中心",
      "f6.body": "6 套内置主题即时切换，也可以导入自己的主题。主题系统与保险库完全隔离。",
      "f6.l1": "Haven Original / Snow / Midnight / Forest / Sand / Lavender",
      "f6.l2": "导入 .haventheme 自定义主题",
      "f6.l3": "预览、应用、删除，损坏自动回退",

      "features.gallery.label": "产品界面",

      "flow.eyebrow": "它是怎么工作的",
      "flow.title": "从你的电脑，到加密的抽屉。",
      "flow.lead": "Haven 不替你保存密码，不替你上传文件，不替你托管密钥。它只是在你的设备上，认真完成加密、存储和解密这几件事。",
      "flow.s1.t": "选择文件",
      "flow.s1.d": "从系统中选择或直接拖入。",
      "flow.s2.t": "本地加密",
      "flow.s2.d": "主密码派生密钥，文件分块加密写入。",
      "flow.s3.t": "存入设备",
      "flow.s3.d": "保存在你指定的保险库目录，不上传。",
      "flow.s4.t": "解锁查看",
      "flow.s4.d": "输入主密码，文件在内存中解密预览。",
      "flow.s5.t": "导出 / 备份",
      "flow.s5.d": "需要时导出明文副本，或整体备份加密保险库。",
      "flow.note": "<b>注意：</b>导出的文件是明文副本，离开 Haven 后不再受加密保护。<b>普通文件删除不等于安全擦除</b>，Haven 不承诺覆写磁盘，也不承诺清除系统级明文痕迹。",

      "sec.eyebrow": "安全与隐私设计",
      "sec.title": "我们认真对待每一次加密。",
      "sec.lead": "下面是 Haven 当前版本采用的实际设计。我们不声称已通过第三方审计，也不声称绝对安全。",
      "sec.l1.code": "Argon2id",
      "sec.l1.t": "主密码派生",
      "sec.l1.d": "256 MiB 内存、5 次迭代、每个保险库独立盐值。",
      "sec.l2.code": "XChaCha20-Poly1305",
      "sec.l2.t": "认证加密",
      "sec.l2.d": "主密钥、加密索引、分块文件全部使用 AEAD。",
      "sec.l3.code": "HKDF-SHA256",
      "sec.l3.t": "密钥分块扩展",
      "sec.l3.d": "从主密钥派生出多用途加密块。",
      "sec.l4.code": "分块加密",
      "sec.l4.t": "大文件写入",
      "sec.l4.d": "文件按块加密写入，避免完整明文一次性驻留内存。",
      "sec.l5.code": "Rust · zeroize",
      "sec.l5.t": "密钥生命周期",
      "sec.l5.d": "主密钥只存在 Rust 后端内存；锁定或退出时 zeroize 清零。",
      "sec.l6.code": "前端无密钥",
      "sec.l6.t": "职责隔离",
      "sec.l6.d": "Vue 前端只拿到状态、路径和错误码，不持有主密钥或派生密钥。",
      "sec.l7.code": "CSP + Tauri",
      "sec.l7.t": "运行时限制",
      "sec.l7.d": "严格 CSP 禁止外部资源与网络请求；Tauri capability 最小权限。",

      "sec.arch.t": "分层安全架构",
      "sec.arch.l1": "主密码",
      "sec.arch.l2": "Argon2id 派生",
      "sec.arch.l3": "HKDF-SHA256 扩展",
      "sec.arch.l4": "XChaCha20-Poly1305 加密",
      "sec.arch.l5": "加密索引 · 分块文件",
      "sec.boundary": "<b>安全边界诚实声明：</b>未通过第三方安全审计。无法清除交换空间、休眠镜像、Spotlight 缓存等系统级痕迹。离线暴力破解的主要防线是 Argon2id 的计算成本，不提供错误次数锁定。",

      "gallery.eyebrow": "产品界面",
      "gallery.title": "一些真实的 Haven。",
      "gallery.lead": "以下是 Haven 当前版本在不同主题、不同状态下的真实截图。",

      "os.eyebrow": "开源",
      "os.title": "源代码公开，欢迎参与改进。",
      "os.lead": "Haven 源代码公开，欢迎开发者阅读代码、提出建议、参与讨论，与社区共同改进项目。许可证尚未确定。",
      "os.github": "GitHub 仓库",
      "os.github.url": "xan-ok/haven",
      "os.issues": "报告问题 / 功能建议",
      "os.pr": "提交 Pull Request",
      "os.readme": "项目 README",
      "os.releases": "发布说明",
      "os.license-note.t": "关于开源许可证",
      "os.license-note.b": "当前仓库根目录尚未添加正式的 LICENSE 文件。Haven 源代码公开，欢迎查看和参与讨论；如需商业使用或再分发，请与作者确认。",

      "os.contrib.t": "参与开发",
      "os.contrib.s1": "阅读 <strong>README</strong> 和项目开发约束，确认 Haven 的安全边界和当前实现。",
      "os.contrib.s2": "在 <strong>Issues</strong> 中浏览已有任务，或先讨论你的功能建议 / Bug 报告。",
      "os.contrib.s3": "Fork 仓库，创建分支（建议 `feature/...` 或 `fix/...`），提交修改。",
      "os.contrib.s4": "发起 <strong>Pull Request</strong>，附上变更说明、测试结果和截图。",
      "os.contrib.s5": "通过代码审查后合并，一起维护这个项目。",

      "dl.eyebrow": "下载",
      "dl.title": "Haven v0.1.3 正式发布。",
      "dl.lead": "Haven v0.1.3 是当前稳定版本，支持 <strong>macOS Apple Silicon（arm64）</strong>。安装包 <strong>已构建但未签名、未公证</strong>，Gatekeeper 首次打开会拒绝。",
      "dl.version": "0.1.3",
      "dl.platform": "macOS Apple Silicon（arm64）",
      "dl.sha": "SHA-256",
      "dl.sha.value": "d7e0e15873b9091253b1eeb5276f88a001b525fd324a97f205af932e65f3ba6f",
      "dl.size": "6.11 MiB",
      "dl.arch": "arm64",
      "dl.build": "2026-11-25T15:34:52 CST",
      "dl.tag": "v0.1.3",
      "dl.commit": "c4a8e3aa",
      "dl.btn.dmg": "下载 DMG（arm64，6.11 MiB）",
      "dl.btn.github": "GitHub Releases",
      "dl.btn.repo": "GitHub 仓库",
      "dl.btn.notes": "Release Notes",
      "dl.sha.copied": "已复制",
      "dl.install.title": "安装说明",
      "dl.install.step1": "1. 下载 DMG（来自 GitHub Releases），校验 SHA-256。",
      "dl.install.step2": "2. 打开 DMG，将 Haven.app 拖入「应用程序」。",
      "dl.install.step3": "3. 首次打开：访达中右键 Haven.app → 选择「打开」→ 确认。",
      "dl.install.hint": "不要关闭系统完整性保护或全局 Gatekeeper。",
      "dl.status.signed": "签名",
      "dl.status.signed.no": "❌ 未签名（构建机无 Developer ID Application 证书）",
      "dl.status.notarized": "公证",
      "dl.status.notarized.no": "❌ 未公证（构建机未配置 notarytool 凭据）",
      "dl.status.gatekeeper": "Gatekeeper",
      "dl.status.gatekeeper.hint": "首次打开会被拒绝，需手动允许",
      "dl.warning": "<strong>诚实说明：</strong>当前安装包未签名、未公证。Gatekeeper 首次打开会拒绝，请在访达中右键 Haven.app → 选择「打开」。这是因为构建机没有 Apple Developer 证书和 notarytool 凭据。许可证尚未确定，如需商业使用或再分发请与作者确认。",

      "faq.eyebrow": "常见问题",
      "faq.title": "也许你还想知道。",
      "faq.q1": "Haven 是什么？",
      "faq.a1": "Haven 是一款本地优先的个人文件加密与隐私管理桌面应用。你可以把照片、文档等文件收进本地加密保险库，用主密码守护它们。当前版本仅支持 macOS Apple Silicon。",
      "faq.q2": "Haven 免费吗？",
      "faq.a2": "源代码公开，欢迎阅读与参与。目前许可证尚未确定，如需使用或再分发请与作者确认。",
      "faq.q3": "Haven 是开源的吗？",
      "faq.a3": "源代码公开，欢迎阅读代码、提出建议、参与讨论。仓库根目录尚未添加正式的 LICENSE 文件，许可证尚未确定。如需商业使用或再分发请与作者确认。",
      "faq.q4": "文件会上传到云端吗？",
      "faq.a4": "不会。Haven 是离线优先的本地应用，不连接任何云端服务器，不自动上传文件，不自动同步到其他设备。",
      "faq.q5": "需要联网使用吗？",
      "faq.a5": "不需要。导入、加密、预览、备份和恢复全部离线完成。",
      "faq.q6": "忘记主密码怎么办？",
      "faq.a6": "<strong>没有找回或重置机制。</strong>主密码派生密钥，密钥加密主密钥，主密钥加密一切。忘记主密码 = 保险库内容无法解密。请务必把主密码记在你自己可信、你自己能拿到的地方。",
      "faq.q7": "支持 Windows / Linux 吗？",
      "faq.a7": "当前版本仅支持 macOS Apple Silicon（arm64）。Intel / Universal 架构、Windows、Linux 均未实现。",
      "faq.q8": "支持 iOS / Android 吗？",
      "faq.a8": "不支持。Haven 是桌面端应用，没有手机版本。",
      "faq.q9": "文件导出后还加密吗？",
      "faq.a9": "<strong>不加密。</strong>导出的文件是未加密明文副本，离开 Haven 后不再受保险库保护。界面会在导出时给出警告。",
      "faq.q10": "删除文件等于安全擦除吗？",
      "faq.a10": "<strong>不等于。</strong>删除只移除保险库内的加密数据，不承诺覆写磁盘，也不承诺清除文件系统、系统备份或存储设备层面的残留。Haven 不声称安全擦除。",
      "faq.q11": "如何备份和恢复？",
      "faq.a11": "在已解锁界面点击「备份保险库」，Haven 会把整个加密保险库完整复制到你选择的目录，备份保持加密状态。恢复时选择备份目录 → 输入主密码完成验证 → 选择新的恢复位置，Haven 会创建新的保险库，不会覆盖原保险库。",
      "faq.q12": "如何参与开发？",
      "faq.a12": "阅读项目 README 和开发约束 → 在 GitHub Issues 浏览或讨论 → Fork 仓库并创建分支 → 提交 Pull Request。详见本站「开源」部分。",

      "footer.about.t": "关于 Haven",
      "footer.about.b": "离线优先的个人文件加密与隐私管理桌面应用。数据保存在本机，不上传、不同步、无需联网。",
      "footer.project.t": "项目",
      "footer.project.readme": "README",
      "footer.project.releases": "发布说明",
      "footer.project.agents": "开发约束（AGENTS.md）",
      "footer.community.t": "社区",
      "footer.community.gh": "GitHub 仓库",
      "footer.community.issues": "Issues",
      "footer.community.pr": "Pull Requests",
      "footer.legal.t": "更多",
      "footer.legal.download": "下载与版本状态",
      "footer.legal.license": "许可证（待定）",
      "footer.copy": "© 2026 Haven · v0.1.3 正式发布 · 源代码公开，欢迎参与"
    },

    en: {
      "nav.features": "Features",
      "nav.security": "Security",
      "nav.open": "Open Source",
      "nav.download": "Download",
      "nav.cta": "GitHub",
      "nav.lang": "中文",

      "hero.eyebrow": "Local-first · Offline · End-to-end",
      "hero.title": "Your files. Your vault. Your <em>control</em>.",
      "hero.lead": "Haven is a local-first encrypted file vault designed to keep your personal files private — right on your own device. No cloud. No accounts. No network.",
      "hero.cta.primary": "View on GitHub",
      "hero.cta.secondary": "Explore Features",
      "hero.meta.local": "Data stays on your device",
      "hero.meta.offline": "Works without network",
      "hero.meta.oss": "Source code public",

      "philosophy.eyebrow": "Why Haven",
      "philosophy.title": "Your files deserve a quiet place.",
      "philosophy.lead": "Today your photos, documents, and personal data are scattered across cloud drives, chat apps, email, and services. Haven gives you <strong>an encrypted local space you control</strong> — no uploads, no sync, no account, no network.",
      "philosophy.p1.title": "No cloud dependency",
      "philosophy.p1.body": "No servers, no APIs, no accounts. You are your own cloud.",
      "philosophy.p2.title": "No sign-up required",
      "philosophy.p2.body": "Launch and use. No email, no phone, no name.",
      "philosophy.p3.title": "Works offline",
      "philosophy.p3.body": "Import, encrypt, preview, backup — all without network.",
      "philosophy.p4.title": "No auto-upload",
      "philosophy.p4.body": "Original files stay where they were. Haven never ships them anywhere.",
      "philosophy.p5.title": "No auto-sync",
      "philosophy.p5.body": "Haven will not silently copy your files to another device.",

      "features.eyebrow": "Core Features",
      "features.title": "A quiet, reliable private drawer.",
      "features.lead": "Haven does a few things, and tries to do them well. Here is what the current version actually implements.",

      "f1.title": "Private Vault",
      "f1.body": "Create independent encrypted vaults protected by a master password only you know. Each vault lives in a directory you choose.",
      "f1.l1": "Independent local vault per location",
      "f1.l2": "Master password → Argon2id derived key",
      "f1.l3": "XChaCha20-Poly1305 AEAD",

      "f2.title": "File Import",
      "f2.body": "Gather files already scattered across your system into your vault.",
      "f2.l1": "Single, multi-file, recursive folder import",
      "f2.l2": "Drag and drop",
      "f2.l3": "Chunk-encrypted write; original files stay in place",

      "f3.title": "Organize",
      "f3.body": "Files in your vault are where you expect them to be.",
      "f3.l1": "List and grid views",
      "f3.l2": "Filename search, favorites, folders",
      "f3.l3": "Rename, move, delete, inspect details",

      "f4.title": "Preview",
      "f4.body": "Haven decrypts files <strong>in memory</strong>. It never leaves a plaintext copy on disk for preview.",
      "f4.l1": "JPG / PNG image preview",
      "f4.l2": "PDF preview",
      "f4.l3": "MP4 / MOV / WebM video preview",

      "f5.title": "Backup & Restore",
      "f5.body": "Back up an entire encrypted vault to another disk, or restore to a new location.",
      "f5.l1": "Backup stays encrypted",
      "f5.l2": "Restore requires master password verification",
      "f5.l3": "Restores to a new location — never overwrites",

      "f6.title": "Theme Center",
      "f6.body": "Six built-in themes, instantly switchable. Import your own. Theme system is fully isolated from vault data.",
      "f6.l1": "Haven Original / Snow / Midnight / Forest / Sand / Lavender",
      "f6.l2": "Import custom .haventheme files",
      "f6.l3": "Preview, apply, delete — auto-fallback on corruption",

      "features.gallery.label": "Product interface",

      "flow.eyebrow": "How it works",
      "flow.title": "From your computer, into an encrypted drawer.",
      "flow.lead": "Haven does not store your password, does not upload your files, and does not host your keys. It simply encrypts, stores, and decrypts — on your own device.",
      "flow.s1.t": "Pick files",
      "flow.s1.d": "Select from system or drag them in.",
      "flow.s2.t": "Encrypt locally",
      "flow.s2.d": "Derive key from master password, write chunks.",
      "flow.s3.t": "Store on device",
      "flow.s3.d": "Saved in your vault directory. No uploads.",
      "flow.s4.t": "Unlock to view",
      "flow.s4.d": "Enter master password. Preview decrypts in memory.",
      "flow.s5.t": "Export / Backup",
      "flow.s5.d": "Export a plaintext copy, or backup the whole vault.",
      "flow.note": "<b>Important:</b> Exported files are plaintext copies and are no longer protected by Haven after export. <b>Deleting a file does not equal secure erasure</b> — Haven does not promise overwrites or system-level trace cleanup.",

      "sec.eyebrow": "Security & Privacy",
      "sec.title": "We take encryption seriously — honestly.",
      "sec.lead": "Below is what Haven currently implements. We do not claim third-party audits or absolute security.",
      "sec.l1.code": "Argon2id",
      "sec.l1.t": "Master password KDF",
      "sec.l1.d": "256 MiB memory, 5 iterations, per-vault salt.",
      "sec.l2.code": "XChaCha20-Poly1305",
      "sec.l2.t": "Authenticated encryption",
      "sec.l2.d": "Primary key, encrypted index, and file chunks all use AEAD.",
      "sec.l3.code": "HKDF-SHA256",
      "sec.l3.t": "Key derivation",
      "sec.l3.d": "Expands the master key into purpose-specific encryption blocks.",
      "sec.l4.code": "Chunked encryption",
      "sec.l4.t": "Large file writes",
      "sec.l4.d": "Chunk-level write avoids holding full plaintext in memory at once.",
      "sec.l5.code": "Rust · zeroize",
      "sec.l5.t": "Key lifecycle",
      "sec.l5.d": "Primary key only lives in Rust backend memory; zeroize on lock or exit.",
      "sec.l6.code": "No keys in frontend",
      "sec.l6.t": "Separation of concerns",
      "sec.l6.d": "Vue frontend only receives state, paths, and error codes — never the master key or derived keys.",
      "sec.l7.code": "CSP + Tauri",
      "sec.l7.t": "Runtime limits",
      "sec.l7.d": "Strict CSP forbids external resources and network requests; minimal Tauri capability permissions.",

      "sec.arch.t": "Layered architecture",
      "sec.arch.l1": "Master password",
      "sec.arch.l2": "Argon2id derivation",
      "sec.arch.l3": "HKDF-SHA256 expand",
      "sec.arch.l4": "XChaCha20-Poly1305 encrypt",
      "sec.arch.l5": "Encrypted index · Chunked files",
      "sec.boundary": "<b>Honest boundary statement:</b> No third-party security audit. Cannot eliminate swap space, hibernate images, Spotlight caches, or other system-level traces. Offline brute-force defense is Argon2id cost; no rate-limiting.",

      "gallery.eyebrow": "Product Gallery",
      "gallery.title": "Haven, from the outside.",
      "gallery.lead": "These are real screenshots of the current version of Haven, across different themes and states.",

      "os.eyebrow": "Open Source",
      "os.title": "Source code public. Contributions welcome.",
      "os.lead": "Haven's source code is public. You are welcome to read it, suggest improvements, and discuss the project with the community. A formal license has not been determined yet.",
      "os.github": "GitHub repository",
      "os.github.url": "xan-ok/haven",
      "os.issues": "Report an issue / Request a feature",
      "os.pr": "Open a Pull Request",
      "os.readme": "Project README",
      "os.releases": "Release notes",
      "os.license-note.t": "About the license",
      "os.license-note.b": "The repository root does not currently contain a formal LICENSE file. Source code is public and open to read and discuss; for commercial use or redistribution, please check with the author.",

      "os.contrib.t": "How to contribute",
      "os.contrib.s1": "Read the <strong>README</strong> and project development constraints. Understand Haven's security boundary and current scope.",
      "os.contrib.s2": "Browse <strong>Issues</strong>, or open a discussion first for feature ideas or bug reports.",
      "os.contrib.s3": "Fork the repository, create a branch (suggested `feature/...` or `fix/...`), commit your changes.",
      "os.contrib.s4": "Open a <strong>Pull Request</strong> with a clear description, tests, and screenshots.",
      "os.contrib.s5": "Merge through code review — let's maintain this project together.",

      "dl.eyebrow": "Download",
      "dl.title": "Haven v0.1.3 — Stable Release.",
      "dl.lead": "Haven v0.1.3 is the current stable version for <strong>macOS Apple Silicon (arm64)</strong>. The installer <strong>has been built but is unsigned and not notarized</strong>. Gatekeeper will refuse it on first open.",
      "dl.version": "0.1.3",
      "dl.platform": "macOS Apple Silicon (arm64)",
      "dl.sha": "SHA-256",
      "dl.sha.value": "d7e0e15873b9091253b1eeb5276f88a001b525fd324a97f205af932e65f3ba6f",
      "dl.size": "6.11 MiB",
      "dl.arch": "arm64",
      "dl.build": "2026-11-25T15:34:52 CST",
      "dl.tag": "v0.1.3",
      "dl.commit": "c4a8e3aa",
      "dl.btn.dmg": "Download DMG (arm64, 6.11 MiB)",
      "dl.btn.github": "GitHub Releases",
      "dl.btn.repo": "GitHub repository",
      "dl.btn.notes": "Release Notes",
      "dl.sha.copied": "Copied",
      "dl.install.title": "How to install",
      "dl.install.step1": "1. Download the DMG from GitHub Releases and verify the SHA-256.",
      "dl.install.step2": "2. Open the DMG, drag Haven.app into Applications.",
      "dl.install.step3": "3. First open: right-click Haven.app in Finder → choose 'Open' → confirm.",
      "dl.install.hint": "Do not disable SIP or global Gatekeeper.",
      "dl.status.signed": "Signature",
      "dl.status.signed.no": "❌ Unsigned (no Developer ID Application certificate on build machine)",
      "dl.status.notarized": "Notarization",
      "dl.status.notarized.no": "❌ Not notarized (no notarytool credentials configured)",
      "dl.status.gatekeeper": "Gatekeeper",
      "dl.status.gatekeeper.hint": "Will refuse on first open — manual allow required",
      "dl.warning": "<strong>Honest note:</strong> The current installer is unsigned and not notarized. Gatekeeper will refuse it on first open — right-click Haven.app in Finder and choose 'Open'. This is because the build machine has no Apple Developer certificate and no notarytool credentials. A formal license has not been determined — please check with the author before commercial use or redistribution.",

      "faq.eyebrow": "FAQ",
      "faq.title": "Questions you might have.",
      "faq.q1": "What is Haven?",
      "faq.a1": "Haven is a local-first desktop application that lets you store photos, documents, and other files in an encrypted vault you control. Current version supports macOS Apple Silicon only.",
      "faq.q2": "Is Haven free?",
      "faq.a2": "Source code is public and open to read and discuss. A formal license has not been determined yet — please check with the author before any use or redistribution.",
      "faq.q3": "Is Haven open source?",
      "faq.a3": "Source code is public. You are welcome to read it, suggest improvements, and discuss the project with the community. The repository root does not contain a formal LICENSE file — a license has not been determined. Please check with the author before commercial use or redistribution.",
      "faq.q4": "Does it upload files to the cloud?",
      "faq.a4": "No. Haven is an offline-first local application. It does not talk to any cloud server, does not auto-upload, and does not auto-sync to other devices.",
      "faq.q5": "Do I need a network connection?",
      "faq.a5": "No. Import, encryption, preview, backup, and restore all work offline.",
      "faq.q6": "I forgot my master password — now what?",
      "faq.a6": "<strong>There is no recovery or reset.</strong> The master password derives the encryption key, which encrypts the primary key, which encrypts everything else. Lost password = vault contents cannot be decrypted. Keep it somewhere only you can access.",
      "faq.q7": "Will Windows / Linux versions be available?",
      "faq.a7": "Not in the current version. Only macOS Apple Silicon (arm64) is supported. Intel / Universal, Windows, and Linux are not implemented.",
      "faq.q8": "Will there be an iOS / Android app?",
      "faq.a8": "No. Haven is desktop-only. There is no mobile version.",
      "faq.q9": "Are exported files still encrypted?",
      "faq.a9": "<strong>No.</strong> Export creates an unencrypted plaintext copy. After export, that file is no longer protected by Haven and the UI will warn you.",
      "faq.q10": "Does deleting a file securely erase it?",
      "faq.a10": "<strong>No.</strong> Deleting removes the encrypted data from the vault. Haven does not promise overwrites or erasure at the filesystem, storage device, or system-level trace level. Haven does not claim secure erasure.",
      "faq.q11": "How do I backup and restore?",
      "faq.a11": "While unlocked, click 'Backup Vault'. Haven copies the entire encrypted vault to your chosen directory — the backup stays encrypted. To restore, pick the backup directory → enter the master password for that backup → choose a new restore location. Haven creates a new vault; it never overwrites an existing one.",
      "faq.q12": "How do I contribute?",
      "faq.a12": "Read the project README and development constraints → browse or discuss in GitHub Issues → fork the repo and create a branch → open a Pull Request. See the 'Open Source' section of this site.",

      "footer.about.t": "About Haven",
      "footer.about.b": "Offline-first personal file encryption and privacy desktop app. Data stays on this machine — no upload, no sync, no network required.",
      "footer.project.t": "Project",
      "footer.project.readme": "README",
      "footer.project.releases": "Release notes",
      "footer.project.agents": "Development rules (AGENTS.md)",
      "footer.community.t": "Community",
      "footer.community.gh": "GitHub repository",
      "footer.community.issues": "Issues",
      "footer.community.pr": "Pull Requests",
      "footer.legal.t": "More",
      "footer.legal.download": "Download and version status",
      "footer.legal.license": "License (to be determined)",
      "footer.copy": "© 2026 Haven · v0.1.3 stable · Source is public — contributions welcome"
    }
  };

  var T = I18N.zh;
  var currentLocale = DEFAULT;

  function applyLocale(locale) {
    if (SUPPORTED.indexOf(locale) === -1) locale = DEFAULT;
    currentLocale = locale;
    T = I18N[locale];
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
    var metaD = document.querySelector('meta[name="description"]');
    if (metaD) metaD.setAttribute("content", T["hero.lead"]);
    var ogD = document.querySelector('meta[property="og:description"]');
    if (ogD) ogD.setAttribute("content", T["hero.lead"]);

    var attr = locale === "zh" ? "data-i18n-zh" : "data-i18n-en";
    var els = document.querySelectorAll("[data-i18n-key]");
    for (var i = 0; i < els.length; i++) {
      var k = els[i].getAttribute("data-i18n-key");
      if (!T[k]) continue;
      if (els[i].tagName === "INPUT" || els[i].tagName === "TEXTAREA" || els[i].tagName === "META") {
        els[i].setAttribute("placeholder", T[k]);
      } else if (els[i].tagName === "TITLE") {
        els[i].textContent = T[k];
      } else {
        els[i].innerHTML = T[k];
      }
    }
  }

  function initLocale() {
    var stored = localStorage.getItem(LOCALE_KEY);
    if (stored && SUPPORTED.indexOf(stored) !== -1) {
      applyLocale(stored);
      return;
    }
    var nav = (navigator.language || navigator.userLanguage || "en").toLowerCase();
    if (nav.indexOf("zh") === 0) applyLocale("zh");
    else applyLocale("en");
  }

  function toggleLocale() {
    var next = currentLocale === "zh" ? "en" : "zh";
    localStorage.setItem(LOCALE_KEY, next);
    applyLocale(next);
  }

  /* ========== Scroll reveal ========== */

  function setupReveal() {
    var nodes = document.querySelectorAll(".reveal");
    if ("IntersectionObserver" in window) {
      var io = new IntersectionObserver(function (entries) {
        entries.forEach(function (e) {
          if (e.isIntersecting) {
            e.target.classList.add("visible");
            io.unobserve(e.target);
          }
        });
      }, { threshold: 0.12, rootMargin: "0px 0px -40px 0px" });
      nodes.forEach(function (n) { io.observe(n); });
    } else {
      nodes.forEach(function (n) { n.classList.add("visible"); });
    }
  }

  /* ========== FAQ accordion ========== */

  function setupFAQ() {
    var items = document.querySelectorAll(".faq-item");
    for (var i = 0; i < items.length; i++) {
      (function (item) {
        var q = item.querySelector(".faq-q");
        if (!q) return;
        q.addEventListener("click", function () {
          var open = item.classList.contains("open");
          if (!open) {
            // close siblings
            var parent = item.parentNode;
            for (var j = 0; j < parent.children.length; j++) {
              parent.children[j].classList.remove("open");
            }
          }
          item.classList.toggle("open");
        });
      })(items[i]);
    }
  }

  /* ========== Lightbox ========== */

  function setupLightbox() {
    var lb = document.createElement("div");
    lb.className = "lightbox";
    lb.innerHTML =
      '<button class="lightbox__close" aria-label="Close">&times;</button>' +
      '<img alt="">';
    document.body.appendChild(lb);
    var img = lb.querySelector("img");
    var closeBtn = lb.querySelector(".lightbox__close");

    function open(src) {
      img.src = src;
      lb.classList.add("open");
      document.body.style.overflow = "hidden";
    }
    function close() {
      lb.classList.remove("open");
      document.body.style.overflow = "";
      img.removeAttribute("src");
    }

    var triggers = document.querySelectorAll(
      ".features-gallery__item img, .gallery-grid__cell img"
    );
    for (var i = 0; i < triggers.length; i++) {
      (function (t) {
        t.style.cursor = "zoom-in";
        t.addEventListener("click", function () {
          open(t.src);
        });
      })(triggers[i]);
    }

    lb.addEventListener("click", function (e) {
      if (e.target === lb) close();
    });
    closeBtn.addEventListener("click", close);
    document.addEventListener("keydown", function (e) {
      if (e.key === "Escape" && lb.classList.contains("open")) close();
    });
  }

  /* ========== Mobile menu ========== */

  function setupMobileMenu() {
    var btn = document.querySelector(".site-nav__burger");
    if (!btn) return;
    var menu = document.querySelector(".mobile-menu");
    if (!menu) return;
    btn.addEventListener("click", function () {
      menu.classList.toggle("open");
      document.body.style.overflow = menu.classList.contains("open") ? "hidden" : "";
    });
    var links = menu.querySelectorAll("a");
    for (var i = 0; i < links.length; i++) {
      links[i].addEventListener("click", function () {
        menu.classList.remove("open");
        document.body.style.overflow = "";
      });
    }
  }

  /* ========== Boot ========== */

  document.addEventListener("DOMContentLoaded", function () {
    initLocale();
    setupReveal();
    setupFAQ();
    setupLightbox();
    setupMobileMenu();

    var toggler = document.getElementById("lang-toggle");
    if (toggler) toggler.addEventListener("click", toggleLocale);
  });
})();
