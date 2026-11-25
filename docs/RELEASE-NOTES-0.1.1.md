# Haven 0.1.1 发布说明（内部测试版）

- 版本：0.1.1
- 平台：macOS Apple Silicon（arm64）
- 产物：`Haven_0.1.1_aarch64.dmg`
- DMG SHA-256：`c1d5b55a934bc1242992979a5dfc193fab6cb75f137113e456878ead8177e782`
- 基线：v0.1.0（`8cb11b66725dc74c22043f71f891e52930217ac4`）

## 本次变更

- 修复「保险库不可用」页面只有「重试」和「重新选择位置」、无法创建新保险库的问题。
- 在该页面新增「创建新保险库」主按钮；原数据丢失或目录为空时，可直接创建新库，无需手动清除位置记忆。
- 保留「重新选择位置」与「重试」，并补充说明：Haven 不会自动覆盖原位置。
- 版本号更新为 0.1.1（前端页脚、`package.json`、`Cargo.toml`、`tauri.conf.json`）。

未改动加密逻辑、保险库格式、会话状态机或 Tauri 权限配置。

## 验证

在隔离 HOME 与临时目录中复现“目录存在但为空”的场景：

1. 界面显示「找不到你的保险库」，同时显示「创建新保险库」「重新选择位置」「重试」三个按钮。
2. 点击「创建新保险库」，选择新父目录，设置主密码后可正常创建。
3. 创建后自动进入已解锁界面，页脚显示 `v0.1.1`，保险库目录生成 `vault.json`、`index.hvn`。

截图与记录：[docs/acceptance/20261125-fix-unavailable-create/](acceptance/20261125-fix-unavailable-create/)

前端 150 项测试通过；`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 通过。

## 安装包状态

- 未使用 Developer ID 签名，未经过 Apple 公证；带 quarantine 属性时 Gatekeeper 可能拒绝运行。
- 安装方式与安全限制同 [0.1.0 发布说明](RELEASE-NOTES-0.1.0.md)：主密码无法找回、删除不等于安全擦除、不承诺清除系统级明文痕迹、不支持网络卷保险库、拔盘非实时监听、休眠/锁屏不立即锁定。
