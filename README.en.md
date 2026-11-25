# Haven

Offline-first personal file encryption and privacy desktop app for macOS. Your data stays on your machine — no upload, no sync, no network required.

> **Current stable release: Haven v0.1.3**, for **macOS Apple Silicon (arm64)**.
> The current installer is **not signed with a Developer ID and not notarized by Apple**; Gatekeeper may refuse to run it. See [Download and Install](#download-and-install) for details.

- Product website: https://37lur66wyyzfjjex5tg.traesite.cn/
- GitHub Releases: https://github.com/xan-ok/haven/releases/tag/v0.1.3
- Git tag: `v0.1.3`
- Product requirements: [docs/Haven-PRD-产品需求分析-v0.2.md](docs/Haven-PRD-产品需求分析-v0.2.md)（Chinese）
- Release notes for 0.1.3: [docs/RELEASE-NOTES-0.1.3.md](docs/RELEASE-NOTES-0.1.3.md)
- Changelog: [CHANGELOG.md](CHANGELOG.md)
- Development rules: [AGENTS.md](AGENTS.md)

## Currently implemented

Implemented:

- Vault creation, master password rules, unlock, manual lock, automatic lock after 5 minutes of inactivity
- Real file import (single file / multi-select / recursive folder / drag and drop) with chunked encrypted writes
- Encrypted index, filename search, list and grid views, image thumbnails
- Favorites, folders (create / rename / delete / move files)
- File rename, delete, details view, integrity verification
- In-memory decrypted preview for images (JPG/PNG), PDF, and video (MP4/MOV/M4V/WebM)
- Single-file, batch, and folder export, with a warning that the output is an unencrypted copy
- Vault backup and restore from backup
- Theme Center: 6 built-in themes with instant switching, import / preview / apply / delete of `.haventheme` custom themes, automatic fallback when a theme is damaged

Not implemented or not supported:

- Developer ID signing and Apple notarization (the build machine has no certificate and no notarytool credentials)
- Intel / Universal builds, Windows, Linux
- Vaults on network volumes
- Master password change or recovery, trash bin, cloud sync

## Platform support backlog

The platforms below are **not developed yet**. This is a backlog record only: work has not started, and no timeline is promised.

| Platform | Status | Notes |
|---|---|---|
| macOS Apple Silicon (arm64) | ✅ Supported | Stable release in v0.1.3; the only usable platform today |
| macOS Intel (x86_64) / Universal | ⏳ Planned | Needs verification on Intel hardware or a cross-compilation setup |
| Windows (x86_64) | ⏳ Planned | Needs Windows-specific path, permission, and installer work |
| Linux (x86_64) | ⏳ Planned | Distribution targets and packaging format still to be decided |
| iOS | ⏳ Planned | Mobile needs a redesigned interaction, file access, and backup model |
| Android | ⏳ Planned | Mobile needs a redesigned interaction, file access, and backup model |

No platform other than macOS Apple Silicon is **downloadable or runnable** at this time. If support for another platform begins, this list and the release notes will be updated before any installer is published.

## Download and install

### System requirements

- macOS Apple Silicon (M1 / M2 / M3 / M4), macOS 13 Ventura or later
- Intel Mac, Windows, and Linux are not supported at this time

### Download

**Haven v0.1.3** stable release:

| Item | Value |
|---|---|
| File name | `Haven_0.1.3_aarch64.dmg` |
| Architecture | arm64 |
| Size | 6.11 MiB |
| SHA-256 | `d7e0e15873b9091253b1eeb5276f88a001b525fd324a97f205af932e65f3ba6f` |
| Git tag | `v0.1.3` |
| Build commit | `c4a8e3aa4435d8a8483d73896bd2341c22a30238` |
| Build time | 2026-11-25T15:34:52 |
| Download | https://github.com/xan-ok/haven/releases/tag/v0.1.3 |

### Integrity check

After downloading, run this in Terminal (`shasum` ships with macOS):

```bash
shasum -a 256 Haven_0.1.3_aarch64.dmg
```

The output must match the SHA-256 above exactly.

### Installation

1. Download `Haven_0.1.3_aarch64.dmg` from GitHub Releases and verify the SHA-256.
2. Open the DMG and drag `Haven.app` into Applications.
3. This build is **unsigned and not notarized**. On first launch macOS reports that the developer cannot be verified. In Finder, right-click `Haven.app` → choose **Open** → confirm.

**Do not disable System Integrity Protection (SIP) or Gatekeeper globally.** Allow this single app only.

### Installer status (honest disclosure)

- **Signature**: not signed with a Developer ID Application certificate (the build machine has no Apple Developer certificate installed).
- **Notarization**: not notarized by Apple (the build machine has no notarytool credentials configured).
- **Gatekeeper**: first launch is refused; the user must allow it manually.

---

## Basic usage

### Create a vault

1. Launch Haven and click "Create Vault".
2. Click "Choose Location" and select a parent directory; Haven creates a fixed-name vault folder inside it.
3. Enter a master password of at least 12 characters, confirm it, and check "I understand that forgetting the master password means no file can be recovered".
4. Click "Create Vault"; the vault is unlocked immediately afterwards.

### Import files

- Click "Import Files" to pick one or more files, or "Import Folder" to recursively import regular files inside it; you can also drag files into the window.
- Import writes chunk by chunk in encrypted form. Original files stay where they are and are never deleted automatically.
- The result reports how many files succeeded and how many failed.

### Search and organize

- The search box at the top filters by filename.
- Switch between "All files / Favorites" and between list and grid views.
- Create folders, move files, rename, delete, and open details.

### Preview

- Click a file row or thumbnail to preview; press `Esc` to close.
- Images: JPG/PNG up to 20 MB. PDF up to 20 MB. Video: MP4/MOV/M4V/WebM up to 64 MB.
- Preview content is decrypted in memory and never written to a temporary plaintext file; files above the limit report that preview is unsupported.

### Export

- Single file: click "Export" on the file row.
- Batch: select several files and click "Batch export"; folders can be exported too.
- **Exported files are unencrypted plaintext copies and are no longer protected by Haven.** The UI shows a warning; store them responsibly.

### Backup and restore

- "Back up vault" copies the vault's encrypted files to a directory you choose. Backups remain encrypted.
- "Restore vault" (or "Restore from backup" on the locked screen): choose the backup directory → enter the master password used when that backup was created → choose a restore location. Haven creates a new vault folder there.
- Restore never overwrites or replaces the existing vault.

### Themes

- On the unlocked screen, click "Appearance" in the top bar to open the Theme Center.
- 6 built-in themes switch instantly (Haven Original / Snow / Midnight / Forest / Sand / Lavender).
- You can import a `.haventheme` custom theme, preview it, then apply it. The theme file format is validated on the Rust side; unknown fields and scripts are rejected.
- Theme preferences live outside the vault and fall back to Haven Original automatically if damaged.

### Lock and quit

- "Lock vault" wipes the backend master key immediately and returns to the locked screen.
- The app locks automatically after 5 minutes without UI interaction. Long tasks such as import, export, or preview do not pause the timer.
- Closing the window quits the app; the next launch requires the master password again.

---

## Security boundaries and known limitations

- The master key exists only in Rust backend memory and is zeroized on lock or exit (`zeroize`). The frontend only receives status, paths, and error information.
- If the master password is forgotten, nothing inside the vault can be decrypted. Haven offers no recovery or reset.
- **Deleting a file is not secure erasure**: deletion only removes encrypted data from the vault. Haven does not overwrite the disk and does not claim to erase leftovers at the filesystem, backup, or storage-device level.
- **Haven does not claim to erase OS-level plaintext traces**, including swap space, hibernation images, crash dumps, system logs, and Spotlight/thumbnail caches.
- Vaults on **network volumes are not supported** (SMB/AFP/NFS etc.). Use a local disk or a locally attached external disk.
- After an external disk is unplugged, Haven **does not watch for removal in real time**; the next vault operation triggers a mount check and fails closed, marking the session unusable. Reconnect the disk and enter the master password again.
- Sleep or screen lock **does not lock Haven immediately**; automatic locking is driven only by the in-app 5-minute inactivity timer.
- There is no lockout after repeated wrong passwords; the main defense against offline brute force is the cost of Argon2id.

---

## Tech stack

| Layer | Technology | Version |
| --- | --- | --- |
| Desktop shell | Tauri | 2.12.7 (CLI 2.12.6) |
| Frontend framework | Vue | 3.5 |
| Frontend language | JavaScript | — |
| UI components | Naive UI | 2.45 |
| Build tool | Vite | 8.3 |
| Backend | Rust | 1.91 (edition 2024) |
| Cryptography | Argon2id / XChaCha20-Poly1305 | argon2 0.6 / chacha20poly1305 0.11 |

## Environment requirements

- macOS (currently macOS Apple Silicon only; Xcode Command Line Tools required)
- Node.js 20 or later
- A stable Rust toolchain (installed via `rustup`)

## Development

```bash
npm install
npm run tauri:dev     # run the desktop app
npm run dev           # frontend only (browser preview, no vault capabilities)
```

## Build

```bash
npm run build         # frontend assets
npm run tauri:build   # macOS .app / .dmg, output under src-tauri/target/release/bundle/
```

## Checks and tests

```bash
npm run build
npm test                                        # frontend pure-function tests (158)
cd src-tauri && cargo check                     # Rust compile check
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo fmt --check
cd src-tauri && cargo test                      # Rust unit tests (265)
```

## Encryption and metadata

On vault creation:

1. The OS CSPRNG generates a 256-bit master key and a 16-byte salt.
2. Argon2id derives a 32-byte key from the master password.
3. XChaCha20-Poly1305 encrypts the master key; the AAD covers every unencrypted metadata field.
4. Files are written in chunks encrypted with XChaCha20-Poly1305; an encrypted index records file metadata and content digests.

Parameters actually used:

| Item | Value |
| --- | --- |
| KDF | Argon2id (v0x13) |
| Memory | 262144 KiB (256 MiB) |
| Iterations | 5 |
| Parallelism | 1 |
| Salt | 16 bytes, random per vault |
| AEAD | XChaCha20-Poly1305 |

Location memory is stored in the app config directory (on macOS usually `~/Library/Application Support/app.haven.desktop/vault-location.json`). It contains only the vault directory path — no password and no key.

---

## Project layout

```
haven/
├── docs/                         Product and release documents
├── haven-website/                Static product website (deployed)
├── src/                          Frontend source (Vue 3 SFC, JavaScript)
│   ├── components/               Create, unlock, file panel, preview and detail components
│   ├── lib/                      Frontend controllers for listing, selection, export, preview, drag and drop
│   └── views/HomeView.vue        Four-state UI and auto-lock
├── tests/                        Frontend pure-function tests (node --test)
├── src-tauri/                    Desktop backend (Rust)
│   ├── src/vault.rs              Password derivation, metadata encryption, atomic writes
│   ├── src/session.rs            Session state, master key lifecycle, mount checks
│   ├── src/file_store.rs         Chunked encrypted files, encrypted index, import, integrity, preview
│   ├── src/backup.rs             Vault backup
│   ├── src/restore.rs            Restore from backup with password verification
│   ├── src/export_txn.rs         Export transactions and staging
│   ├── src/export_staging.rs     Export staging directory and volume consistency checks
│   ├── src/theme_store.rs        Theme Center storage and validation
│   └── capabilities/             Permission configuration (least privilege)
├── CHANGELOG.md                  Changelog
├── AGENTS.md                     Development rules
├── README.md                     Chinese README
└── README.en.md                  This file
```

---

## Version history

| Version | Git tag | Notes |
|---|---|---|
| v0.1.3 | `v0.1.3` | **Current stable release.** Theme Center: 6 built-in themes, custom `.haventheme` import, themes fully isolated from the vault, automatic fallback when damaged. |
| v0.1.2 | `v0.1.2` | Internal test build. Unlocked-screen layout adjustments: the file list fills the remaining window height, the top toolbar is more compact. |
| v0.1.1 | `v0.1.1` | Internal test build. Fixed the "vault unavailable" screen being unable to create a new vault. |
| v0.1.0 | `v0.1.0` | Internal test build. Core vault functionality: create, import, encrypt, preview, backup, and restore all delivered. |

## License

No license has been chosen yet. The source code is publicly visible, but no permission to copy, modify, or redistribute it has been granted.
