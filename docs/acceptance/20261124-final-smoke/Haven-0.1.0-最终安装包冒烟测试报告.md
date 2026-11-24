# Haven 0.1.0 最终安装包冒烟测试报告

- 测试日期：2026-11-24
- 测试对象：`src-tauri/target/release/bundle/dmg/Haven_0.1.0_aarch64.dmg`
- 源码冻结提交：`143094066334311cb61529aa45988556bf276102`（分支 `main`）
- 测试方式：临时 HOME + 临时安装目录 + 临时保险库，全程未接触真实保险库

## 一、测试对象与哈希复核

| 项目 | 值 |
| --- | --- |
| DMG 路径 | `src-tauri/target/release/bundle/dmg/Haven_0.1.0_aarch64.dmg` |
| DMG 大小 | 6,311,682 字节 |
| DMG SHA-256 | `602ddc373a7a72ca6b896a44040fc32e35f319632436acd603ad721f81c5a781` |
| 预期 SHA-256 | `602ddc373a7a72ca6b896a44040fc32e35f319632436acd603ad721f81c5a781` |
| 结论 | 一致，未测试错包 |

DMG 挂载正常（`/dev/disk12s1`，Apple_HFS），卷内含 `Haven.app` 与到 `/Applications` 的符号链接。

## 二、测试环境

| 项目 | 值 |
| --- | --- |
| macOS | 26.5（Build 25F71） |
| CPU | Apple M2 Pro（arm64） |
| 屏幕 | 3024 × 1964 Retina |
| Rust | rustc 1.91.0-nightly (0060d5a2a 2025-08-04) |
| Node / npm | v26.13.1 / 11.20.0 |
| 临时 HOME | `/tmp/haven-smoke/home` |
| 临时安装目录 | `/tmp/haven-smoke/Applications/Haven.app` |
| 临时保险库 | `/tmp/haven-smoke/vault/我的保险库` |
| 恢复目标库 | `/tmp/haven-smoke/data/restore/我的保险库` |
| 测试密码 | 临时测试密码，仅存在于已删除的临时目录，不在验收材料中保留 |

## 三、安装与首次启动

### 3.1 安装

1. `hdiutil attach -nobrowse -readonly` 挂载最终 DMG。
2. `ditto` 将 `Haven.app` 复制到 `/tmp/haven-smoke/Applications/`，模拟拖入 Applications。
3. 复制后主程序 SHA-256 与构建产物一致：
   - 主程序：`Contents/MacOS/haven`，13,112,432 字节
   - SHA-256：`366a669528256f9238ac1d35b2c51f994a48bfb90139e82d3ec7345512393da4`
   - 架构：`Mach-O 64-bit executable arm64`（thin）

### 3.2 签名与公证检查

`codesign -dvvv`：

```
Identifier=haven-b5d1cb1848e1bae5
Format=app bundle with Mach-O thin (arm64)
CodeDirectory v=20400 size=101784 flags=0x20002(adhoc,linker-signed) ...
Signature=adhoc
Info.plist=not bound
TeamIdentifier=not set
Sealed Resources=none
Internal requirements=none
```

`codesign --verify --deep --strict --verbose=2`：

```
/tmp/haven-smoke/Applications/Haven.app: code has no resources but signature indicates they must be present
exit=1
```

`spctl -a -t exec -vv`：

```
/tmp/haven-smoke/Applications/Haven.app: code has no resources but signature indicates they must be present
exit=1
```

结论：

- 产物为 **adhoc / linker-signed**，没有 Developer ID 签名，未公证。
- 签名校验确实失败（`Sealed Resources=none` 但签名声明存在资源），与此前报告一致。
- **签名校验失败不等于应用无法运行**：应用可正常启动并完成全部业务操作。

### 3.3 Gatekeeper 首次启动

- 从 DMG 复制出来的 App 只带 `com.apple.provenance`，**不含 `com.apple.quarantine`**（本机构建产物），因此首次启动不会被 Gatekeeper 弹窗拦截。
- 为记录 Gatekeeper 的策略判定，在**临时副本**上人工写入 `com.apple.quarantine` 后执行评估：`spctl --assess` 拒绝（输出同上）；随后删除该扩展属性完成受控首次启动。
- 未关闭系统完整性保护、未关闭全局 Gatekeeper、未修改签名或构建产物。

### 3.4 启动结果

- 窗口标题 `Haven`，界面名称 `Haven`，页脚显示 `v0.1.0`。
- Info.plist：`CFBundleName=Haven`、`CFBundleShortVersionString=0.1.0`、`CFBundleVersion=0.1.0`、`CFBundleIdentifier=app.haven.desktop`、`LSMinimumSystemVersion=10.13`。
- 启动、使用、退出全过程无崩溃；标准错误仅有 1 条系统级 `IMKCFRunLoopWakeUpReliable` 提示（macOS 输入法框架日志，与应用逻辑无关），退出码 0。
- 截图：`screenshots/01-first-launch.png`

## 四、业务冒烟逐项结果

| 序号 | 项目 | 结果 | 证据 |
| --- | --- | --- | --- |
| 1 | 创建保险库 | 通过：在 `/tmp/haven-smoke/vault` 创建「我的保险库」，提示「保险库已创建并解锁」 | `02-vault-created.png` |
| 2 | 导入测试文件 | 通过：一次导入 5 个文件，提示「本次导入：成功 5 个」 | `03-imported.png` |
| 3 | 搜索文件 | 通过：搜索 `sunset` 后由 5 个筛选为 1 个 | `04-search.png` |
| 4 | 预览图片 | 通过：点击 `sunset.jpg` 打开图片预览弹窗，内存解密显示正常 | `05-preview-sunset.png` |
| 5 | 单文件导出 | 通过：导出 `sunset.jpg`、`notes.txt`，成功后明确提示「导出文件是未加密副本，已不再受 Haven 保护，请妥善保管。」 | `06-export-single-warning.png` |
| 6 | 批量导出 | 通过：全选 5 个文件批量导出，提示「批量导出完成：已导出 5 个文件。导出文件是未加密副本，已不再受 Haven 保护……」 | `07-export-batch-warning.png` |
| 7 | 备份保险库 | 通过：备份到 `/tmp/haven-smoke/data/backup/haven-backup-1795513641`，提示「备份完成：5 个加密文件，共 158.6 KB」，备份保持加密状态 | `08-backup.png` |
| 8 | 使用备份恢复 | 通过：选择备份目录 → 输入备份主密码验证 → 选择恢复位置，提示「恢复完成：5 个加密文件，共 158.6 KB」「保险库已恢复到新位置：5 个加密文件」 | `09-restore.png` |
| 9 | 恢复后数量与内容 | 通过：锁定后通过「打开其他保险库」打开恢复库，输入同一主密码解锁，显示 `全部文件 5 个，当前筛选出 5 个`，逐个导出后与源文件哈希一致 | `11-restored-vault.png` |
| 10 | 锁定保险库 | 通过：点击「锁定保险库」进入锁定态，显示「已锁定 / 输入主密码」 | `10-locked.png`、`12-lock-before-quit.png` |
| 11 | 正常退出 | 通过：Cmd+Q 正常退出，进程结束，退出码 0 | 进程记录 |
| 12 | 重启后仍锁定 | 通过：重新启动后直接显示锁定页，需重新输入主密码，不自动解锁 | `13-relaunch-locked.png` |

## 五、导出文件哈希核对

源文件（`/tmp/haven-smoke/data/input`）、批量导出（`batch`）、恢复库导出（`restore-export`）三方 SHA-256 全部一致：

| 文件 | 源文件 SHA-256 | 批量导出 | 恢复库导出 |
| --- | --- | --- | --- |
| `photo.png` | `ed54985ef2306aa682ef9b742f258e9293987a9f1a78fd5626ef074e998a9b45` | 一致 | 一致 |
| `sunset.jpg` | `ce678211466f9fe1dc3f31212f5f7ff85e924bf12486b7a7d26c6b410470bccf` | 一致 | 一致 |
| `clip.mp4` | `64d99166e82ef1bf71a72d748289e5c5423dcfbc1692dca50b6e913bea4163db` | 一致 | 一致 |
| `notes.txt` | `a38750bc988e0af4e378af6b0f6406930705ff026dba0f00d478c60affc30171` | 一致 | 一致 |
| `2026年最终验收测试图片超长中文文件名称示例.png` | `0556cfaed4e418e4f33ed5fd616e4f90810d9a563b7c4560a165778ea9755932` | 一致 | 一致 |

单文件导出同样一致：

- `export/notes.txt` = `a38750bc988e0af4e378af6b0f6406930705ff026dba0f00d478c60affc30171`（与源文件一致）
- `export/sunset.jpg` = `ce678211466f9fe1dc3f31212f5f7ff85e924bf12486b7a7d26c6b410470bccf`（与源文件一致）

## 六、隔离与真实保险库保护

- 应用以 `HOME=/tmp/haven-smoke/home CFFIXED_USER_HOME=/tmp/haven-smoke/home` 启动，配置只写入临时 HOME：
  `.../app.haven.desktop/vault-location.json` → `{"vaultDir": "/private/tmp/haven-smoke/data/restore/我的保险库"}`
- 真实配置 `~/Library/Application Support/app.haven.desktop/vault-location.json` 的修改时间测试前后均为 `2026-11-24 15:29`，未被打开或修改。
- 未使用真实文件、真实备份或真实密码，未解锁、读取或修改真实保险库。

## 七、遗留问题与建议

1. **签名/公证**：产物为 adhoc 签名，`codesign --verify --deep --strict` 与 `spctl` 均失败。当前不影响本机运行，但对外分发会触发 Gatekeeper 拦截，建议正式发布前补 Developer ID 签名与公证。
2. **签名失败的具体原因**：`Sealed Resources=none` 与签名声明不一致，属于打包链路生成的签名不完整，而非二进制损坏；本轮按要求未修改构建产物。
3. 本轮未覆盖 PDF 预览、Intel / Universal 架构与 macOS 10.13 实机验证（不在本轮范围）。

## 八、证据清单

> 说明：截图已裁剪到 Haven 应用窗口区域，仅为了移除与测试无关的桌面内容；未修改任何界面状态或测试结论。测试密码等临时信息未保留在文档中。

`screenshots/`：

1. `01-first-launch.png` —— 首次启动落地页（Haven / v0.1.0）
2. `02-vault-created.png` —— 保险库创建成功
3. `03-imported.png` —— 导入 5 个文件成功
4. `04-search.png` —— 搜索筛选结果
5. `05-preview-sunset.png` —— 图片预览
6. `06-export-single-warning.png` —— 单文件导出未加密副本警告
7. `07-export-batch-warning.png` —— 批量导出未加密副本警告
8. `08-backup.png` —— 备份完成
9. `09-restore.png` —— 恢复完成
10. `10-locked.png` —— 锁定态
11. `11-restored-vault.png` —— 恢复库解锁后 5 个文件
12. `12-lock-before-quit.png` —— 退出前锁定
13. `13-relaunch-locked.png` —— 重启后仍为锁定态
