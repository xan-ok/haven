//! FR-45/FR-46 保险库恢复：把 FR-44 备份原样复制为一个新的独立保险库目录。
//!
//! 安全边界：
//! - 只复制 `vault.json`、`index.hvn` 与索引中登记的密文文件，不复制锁文件、
//!   `tmp/` 或其它运行时缓存。
//! - FR-46：仅凭备份目录与该备份的主密码即可恢复；密码用备份自身的
//!   `vault.json` 参数与现有解锁逻辑验证，不要求备份属于当前会话。
//! - 不解密、不重新加密、不修改任何密文；索引只在内存中解密用于校验完整性。
//! - 备份目录全程只读；目标目录以临时目录完整写入、校验、同步后再原子提交。
//! - 失败或取消只清理本次创建的临时目录，绝不删除或修改其它目录。
//! - 恢复目标是父目录下固定名称 `我的保险库`，已存在时直接拒绝，不覆盖。
//! - 主密码与派生密钥只存在于调用栈内，不落盘、不写日志。

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use serde::Serialize;
use zeroize::Zeroizing;

use crate::backup;
use crate::file_store::{self, Index, IndexEntry};
use crate::vault::{self, VAULT_DIR_NAME, VaultError, random_suffix, unix_now};

const COPY_BUFFER_BYTES: usize = 64 * 1024;
const RESTORE_TEMP_PREFIX: &str = ".haven-restore-";

/// 恢复结果：只包含目标位置、密文数量与总字节数，不包含任何文件名。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSummary {
    pub destination: String,
    pub file_count: u64,
    pub total_bytes: u64,
}

/// 恢复进度：仅在复制密文阶段按文件推进。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreProgress {
    pub processed: u64,
    pub total: u64,
    pub bytes: u64,
}

fn restore_source_invalid() -> VaultError {
    VaultError::new(
        "RESTORE_SOURCE_INVALID",
        "备份位置不可用或不是有效的保险库备份，请重新选择。",
    )
}

fn restore_target_invalid() -> VaultError {
    VaultError::new("RESTORE_TARGET_INVALID", "恢复位置不可用，请选择其他目录。")
}

fn restore_target_exists() -> VaultError {
    VaultError::new(
        "RESTORE_TARGET_EXISTS",
        "目标位置已存在「我的保险库」，Haven 不会覆盖已有目录。",
    )
}

fn restore_password_invalid() -> VaultError {
    VaultError::new(
        "RESTORE_PASSWORD_INVALID",
        "主密码不正确，无法读取该备份。请输入创建该备份的保险库的主密码。",
    )
}

fn restore_incomplete() -> VaultError {
    VaultError::new(
        "RESTORE_INCOMPLETE",
        "备份数据不完整或结构异常，已中止恢复。",
    )
}

fn restore_index_invalid() -> VaultError {
    VaultError::new(
        "RESTORE_INDEX_INVALID",
        "备份中的文件索引无法读取，备份可能不完整或已损坏。",
    )
}

fn restore_cancelled() -> VaultError {
    VaultError::new(
        "RESTORE_CANCELLED",
        "恢复已取消，本次创建的临时目录已清理。",
    )
}

fn restore_failed() -> VaultError {
    VaultError::new("RESTORE_FAILED", "恢复失败，本次创建的临时目录已清理。")
}

/// 用备份自身的 `vault.json` 参数与现有解锁逻辑派生并验证主密钥。
///
/// 复用 `unlock_vault_dir`，不实现第二套密码校验；错误密码返回明确错误码。
fn derive_backup_master_key(
    backup_dir: &Path,
    password: &str,
) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    vault::unlock_vault_dir(backup_dir, password).map_err(|error| match error.code {
        "AUTH_FAILED" => restore_password_invalid(),
        "METADATA_MISSING" | "METADATA_INVALID" | "UNSUPPORTED_VERSION" => restore_incomplete(),
        _ => restore_source_invalid(),
    })
}

fn map_index_error(error: VaultError) -> VaultError {
    match error.code {
        "INDEX_INVALID" | "INDEX_MISSING" => restore_index_invalid(),
        _ => restore_incomplete(),
    }
}

/// FR-46：仅凭备份目录与主密码做只读验证。
///
/// 按现有格式验证密码与索引可读性，不解密任何文件内容，不创建任何目录。
pub(crate) fn verify_backup_password(backup_dir: &Path, password: &str) -> Result<(), VaultError> {
    if !is_plain_dir(backup_dir) {
        return Err(restore_source_invalid());
    }
    let master_key = derive_backup_master_key(backup_dir, password)?;
    Index::load(backup_dir, &master_key)
        .map(|_| ())
        .map_err(map_index_error)
}

/// 恢复入口：使用当前时间与随机后缀生成唯一的临时目录名。
///
/// `current_vault_dir` 是当前打开的保险库（锁定或解锁均可，也可能为无）；
/// 只用于防止恢复目标落进当前保险库，恢复本身不依赖当前会话的密钥。
pub(crate) fn restore_backup(
    backup_dir: &Path,
    target_parent: &Path,
    current_vault_dir: Option<&Path>,
    password: &str,
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(RestoreProgress),
) -> Result<RestoreSummary, VaultError> {
    let timestamp = unix_now()?;
    let suffix = random_suffix()?;
    restore_backup_at(
        backup_dir,
        target_parent,
        current_vault_dir,
        password,
        timestamp,
        &suffix,
        is_cancelled,
        progress,
    )
}

#[allow(clippy::too_many_arguments)]
fn restore_backup_at(
    backup_dir: &Path,
    target_parent: &Path,
    current_vault_dir: Option<&Path>,
    password: &str,
    timestamp: u64,
    suffix: &str,
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(RestoreProgress),
) -> Result<RestoreSummary, VaultError> {
    if !is_plain_dir(backup_dir) {
        return Err(restore_source_invalid());
    }
    if !is_plain_dir(target_parent) {
        return Err(restore_target_invalid());
    }

    let backup_canonical = fs::canonicalize(backup_dir).map_err(|_| restore_source_invalid())?;
    let target_canonical = fs::canonicalize(target_parent).map_err(|_| restore_target_invalid())?;

    // 备份源不得是正在使用的保险库本身；目标不得落在备份目录或当前保险库内，
    // 以免修改原备份或污染当前保险库。
    if let Some(current_vault_dir) = current_vault_dir {
        if !is_plain_dir(current_vault_dir) {
            return Err(VaultError::vault_unavailable());
        }
        let vault_canonical =
            fs::canonicalize(current_vault_dir).map_err(|_| VaultError::vault_unavailable())?;
        if backup_canonical == vault_canonical {
            return Err(restore_source_invalid());
        }
        if target_canonical.starts_with(&vault_canonical) {
            return Err(restore_target_invalid());
        }
    }
    if target_canonical.starts_with(&backup_canonical) {
        return Err(restore_target_invalid());
    }

    let destination = target_parent.join(VAULT_DIR_NAME);
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(restore_target_exists()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(restore_target_invalid()),
    }

    // 备份必需文件：元数据与索引必须存在且都是普通文件。
    let metadata_path = backup_dir.join(vault::METADATA_FILE);
    let index_path = backup_dir.join(file_store::INDEX_FILE);
    regular_file_size(&metadata_path)?;
    regular_file_size(&index_path)?;

    // 密码验证发生在创建任何目录之前：错误密码不会留下临时或最终目录。
    // 索引在内存中解密，用于取得稳定 ID 并确认索引可读；失败说明备份索引损坏。
    let master_key = derive_backup_master_key(backup_dir, password)?;
    let index = Index::load(backup_dir, &master_key).map_err(map_index_error)?;

    let mut expected: BTreeMap<&str, &IndexEntry> = BTreeMap::new();
    for entry in &index.entries {
        expected.insert(entry.id.as_str(), entry);
    }

    // 逐项核对密文：索引登记的每个稳定 ID 都必须存在，结构必须与索引一致。
    for entry in expected.values() {
        let path = file_store::ciphertext_path(backup_dir, &entry.id);
        file_store::validate_ciphertext_structure(&path, entry)
            .map_err(|_| restore_incomplete())?;
    }
    // 反向核对：备份目录中不得出现未登记的文件、符号链接或异常层级。
    let scanned = scan_backup_ciphertexts(backup_dir)?;
    if scanned.len() != expected.len()
        || scanned.iter().any(|id| !expected.contains_key(id.as_str()))
    {
        return Err(restore_incomplete());
    }

    let temp = target_parent.join(format!("{RESTORE_TEMP_PREFIX}{timestamp}-{suffix}"));
    match backup::create_private_dir(&temp) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Err(restore_failed()),
        Err(_) => return Err(restore_target_invalid()),
    }

    let total_files = expected.len() as u64;
    let mut processed = 0u64;
    let mut copied_bytes = 0u64;

    let outcome = (|| -> Result<(), VaultError> {
        backup::ensure_private_dir(&temp.join(file_store::FILES_DIR))
            .map_err(|_| restore_failed())?;

        copied_bytes += copy_regular_file(&metadata_path, &temp.join(vault::METADATA_FILE))?;
        copied_bytes += copy_regular_file(&index_path, &temp.join(file_store::INDEX_FILE))?;
        progress(RestoreProgress {
            processed,
            total: total_files,
            bytes: copied_bytes,
        });

        for entry in expected.values() {
            if is_cancelled() {
                return Err(restore_cancelled());
            }
            let source = file_store::ciphertext_path(backup_dir, &entry.id);
            let target = file_store::ciphertext_path(&temp, &entry.id);
            if let Some(parent) = target.parent() {
                backup::ensure_private_dir(parent).map_err(|_| restore_failed())?;
            }
            copied_bytes += copy_regular_file(&source, &target)?;
            processed += 1;
            progress(RestoreProgress {
                processed,
                total: total_files,
                bytes: copied_bytes,
            });
        }

        sync_tree(&temp);
        Ok(())
    })();

    match outcome {
        Ok(()) => {
            if is_cancelled() {
                let _ = fs::remove_dir_all(&temp);
                return Err(restore_cancelled());
            }
            // 提交前再次确认目标不存在，避免并发创建导致覆盖。
            match fs::symlink_metadata(&destination) {
                Ok(_) => {
                    let _ = fs::remove_dir_all(&temp);
                    return Err(restore_target_exists());
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {
                    let _ = fs::remove_dir_all(&temp);
                    return Err(restore_target_invalid());
                }
            }
            match rename_exclusive(&temp, &destination) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    let _ = fs::remove_dir_all(&temp);
                    return Err(restore_target_exists());
                }
                Err(_) => {
                    let _ = fs::remove_dir_all(&temp);
                    return Err(restore_failed());
                }
            }
            backup::sync_dir(target_parent);
            Ok(RestoreSummary {
                destination: destination.display().to_string(),
                file_count: total_files,
                total_bytes: copied_bytes,
            })
        }
        Err(error) => {
            // 只删除本次创建的临时目录；原备份与当前保险库不受影响。
            let _ = fs::remove_dir_all(&temp);
            Err(error)
        }
    }
}

fn is_plain_dir(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    matches!(
        fs::symlink_metadata(path),
        Ok(info) if info.is_dir() && !info.file_type().is_symlink()
    )
}

fn regular_file_size(path: &Path) -> Result<u64, VaultError> {
    let info = fs::symlink_metadata(path).map_err(|_| restore_incomplete())?;
    if info.file_type().is_symlink() || !info.is_file() {
        return Err(restore_incomplete());
    }
    Ok(info.len())
}

fn is_hex_prefix(name: &str) -> bool {
    name.len() == 2
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 扫描备份的 `files/` 目录，只接受 `<2 位十六进制>/<32 位稳定 ID>.hvn`。
///
/// 拒绝符号链接、特殊文件、额外层级与不符合预期的文件名；返回按 ID 排序的列表。
fn scan_backup_ciphertexts(backup_dir: &Path) -> Result<Vec<String>, VaultError> {
    let files_dir = backup_dir.join(file_store::FILES_DIR);
    let info = fs::symlink_metadata(&files_dir).map_err(|_| restore_incomplete())?;
    if info.file_type().is_symlink() || !info.is_dir() {
        return Err(restore_incomplete());
    }

    let mut ids = Vec::new();
    for bucket in fs::read_dir(&files_dir).map_err(|_| restore_incomplete())? {
        let bucket = bucket.map_err(|_| restore_incomplete())?;
        let bucket_name = bucket.file_name();
        let bucket_name = bucket_name.to_str().ok_or_else(restore_incomplete)?;
        if !is_hex_prefix(bucket_name) {
            return Err(restore_incomplete());
        }
        let bucket_type = bucket.file_type().map_err(|_| restore_incomplete())?;
        if bucket_type.is_symlink() || !bucket_type.is_dir() {
            return Err(restore_incomplete());
        }

        for item in fs::read_dir(bucket.path()).map_err(|_| restore_incomplete())? {
            let item = item.map_err(|_| restore_incomplete())?;
            let file_name = item.file_name();
            let file_name = file_name.to_str().ok_or_else(restore_incomplete)?;
            let id = file_name
                .strip_suffix(".hvn")
                .ok_or_else(restore_incomplete)?;
            if !file_store::valid_file_id(id) || &id[..2] != bucket_name {
                return Err(restore_incomplete());
            }
            let item_type = item.file_type().map_err(|_| restore_incomplete())?;
            if item_type.is_symlink() || !item_type.is_file() {
                return Err(restore_incomplete());
            }
            ids.push(id.to_string());
        }
    }
    ids.sort();
    Ok(ids)
}

/// 只复制普通文件，拒绝符号链接与特殊文件；目标必须以 create_new 打开。
fn copy_regular_file(source: &Path, target: &Path) -> Result<u64, VaultError> {
    let mut input = backup::open_regular_file(source).map_err(|_| restore_incomplete())?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(target).map_err(|_| restore_failed())?;

    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        let read = input.read(&mut buffer).map_err(|_| restore_failed())?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|_| restore_failed())?;
        total += read as u64;
    }
    output.sync_all().map_err(|_| restore_failed())?;
    Ok(total)
}

/// 尽力同步临时目录树，降低提交后崩溃留下未落盘数据的风险。
fn sync_tree(root: &Path) {
    backup::sync_dir(root);
    let files_dir = root.join(file_store::FILES_DIR);
    backup::sync_dir(&files_dir);
    if let Ok(entries) = fs::read_dir(&files_dir) {
        for entry in entries.flatten() {
            backup::sync_dir(&entry.path());
        }
    }
}

/// 原子提交：目标已存在时拒绝，绝不覆盖已有目录。
#[cfg(target_os = "macos")]
fn rename_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path"))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path"))?;
    // RENAME_EXCL 在目标已存在时失败，避免 rename 覆盖空目录。
    let result = unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn rename_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    if fs::symlink_metadata(target).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "exists"));
    }
    fs::rename(source, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::{BackupProgress, backup_vault};
    use crate::file_store::{
        ImportedFile, TMP_DIR, ciphertext_path, create_folder, import_batch, list_folders,
        move_files, prepare_vault, set_favorite,
    };
    use crate::vault::{create_vault_dir, unlock_vault_dir};
    use std::collections::BTreeMap as SnapshotMap;
    use std::fs::File;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use tempfile::TempDir;

    const PASSWORD: &str = "correct horse battery";

    fn setup() -> (TempDir, PathBuf, [u8; 32]) {
        let temp = TempDir::new().expect("temp");
        let (vault_dir, key) = create_vault_dir(temp.path(), PASSWORD).expect("create vault");
        prepare_vault(&vault_dir, &key).expect("prepare");
        (temp, vault_dir, *key)
    }

    fn write_source(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        fs::create_dir_all(dir).expect("dir");
        let path = dir.join(name);
        let mut file = File::create(&path).expect("file");
        file.write_all(bytes).expect("write");
        path
    }

    fn import(vault: &Path, key: &[u8; 32], paths: &[PathBuf]) -> Vec<ImportedFile> {
        let paths = paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        import_batch(vault, key, &paths, &|| false, &|_| {})
            .expect("import")
            .succeeded
    }

    fn no_progress(_: RestoreProgress) {}

    fn make_backup(temp: &TempDir, vault: &Path, key: &[u8; 32]) -> PathBuf {
        let root = temp.path().join("backup-root");
        fs::create_dir(&root).expect("backup root");
        let summary =
            backup_vault(vault, key, &root, &|| false, &|_: BackupProgress| {}).expect("backup");
        PathBuf::from(summary.destination)
    }

    fn snapshot(dir: &Path) -> SnapshotMap<String, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, out: &mut SnapshotMap<String, Vec<u8>>) {
            for entry in fs::read_dir(dir).expect("read dir").flatten() {
                let path = entry.path();
                let info = fs::symlink_metadata(&path).expect("metadata");
                if info.is_dir() {
                    walk(root, &path, out);
                } else {
                    let relative = path
                        .strip_prefix(root)
                        .expect("relative")
                        .to_string_lossy()
                        .to_string();
                    out.insert(relative, fs::read(&path).expect("read"));
                }
            }
        }
        let mut out = SnapshotMap::new();
        walk(dir, dir, &mut out);
        out
    }

    fn target_root(temp: &TempDir) -> PathBuf {
        let root = temp.path().join("restore-root");
        fs::create_dir(&root).expect("restore root");
        root
    }

    fn assert_no_temp_left(root: &Path) {
        let entries: Vec<_> = fs::read_dir(root)
            .expect("read root")
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert!(
            entries
                .iter()
                .all(|name| !name.to_string_lossy().starts_with(RESTORE_TEMP_PREFIX)),
            "unexpected leftovers: {entries:?}"
        );
    }

    #[test]
    fn restores_complete_backup_and_restored_vault_unlocks() {
        let (temp, vault, key) = setup();
        let sources = temp.path().join("sources");
        let first = write_source(&sources, "a.txt", b"alpha");
        let second = write_source(&sources, "b.txt", b"beta");
        let imported = import(&vault, &key, &[first, second]);
        let folder = create_folder(&vault, &key, "资料").expect("folder");
        move_files(
            &vault,
            &key,
            std::slice::from_ref(&imported[0].id),
            Some(&folder.id),
        )
        .expect("move");
        set_favorite(&vault, &key, &imported[0].id, true).expect("favorite");

        let backup_dir = make_backup(&temp, &vault, &key);
        let backup_snapshot = snapshot(&backup_dir);
        let root = target_root(&temp);

        let summary = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect("restore");
        let restored = PathBuf::from(&summary.destination);

        assert_eq!(restored, root.join(VAULT_DIR_NAME));
        assert_eq!(summary.file_count, 2);
        assert!(summary.total_bytes > 0);
        // 恢复结果只包含必需数据，不包含锁文件或临时目录。
        assert!(!restored.join(".haven.lock").exists());
        assert!(!restored.join(TMP_DIR).exists());

        // 恢复后的保险库可直接走现有解锁流程。
        unlock_vault_dir(&restored, PASSWORD).expect("restored vault unlocks");
        let restored_index = Index::load(&restored, &key).expect("restored index");
        assert_eq!(restored_index.entries.len(), 2);
        assert_eq!(restored_index.folders.len(), 1);
        assert_eq!(restored_index.folders[0].id, folder.id);
        assert_eq!(restored_index.folders[0].name, "资料");
        assert_eq!(list_folders(&restored, &key).expect("folders").len(), 1);

        // 文件 ID、名称、收藏与文件夹归属全部保留。
        let entry = restored_index
            .entries
            .iter()
            .find(|entry| entry.id == imported[0].id)
            .expect("restored favorite");
        assert_eq!(entry.name, imported[0].name);
        assert_eq!(entry.size, imported[0].size);
        assert!(entry.favorite);
        assert_eq!(entry.folder_id.as_deref(), Some(folder.id.as_str()));

        // 密文逐字节一致；备份目录内容保持不变。
        for entry in &restored_index.entries {
            assert_eq!(
                fs::read(ciphertext_path(&backup_dir, &entry.id)).expect("backup cipher"),
                fs::read(ciphertext_path(&restored, &entry.id)).expect("restored cipher")
            );
        }
        assert_eq!(snapshot(&backup_dir), backup_snapshot);
        assert_no_temp_left(&root);
        assert_eq!(fs::read_dir(&root).expect("root").count(), 1);

        // 当前保险库仍然可用。
        let original = Index::load(&vault, &key).expect("original index");
        assert_eq!(original.entries.len(), 2);
    }

    #[test]
    fn refuses_existing_destination_without_touching_it() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        let root = target_root(&temp);
        let existing = root.join(VAULT_DIR_NAME);
        fs::create_dir(&existing).expect("existing");
        fs::write(existing.join("keep.txt"), b"keep").expect("keep");

        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("must refuse");
        assert_eq!(error.code, "RESTORE_TARGET_EXISTS");
        assert_eq!(fs::read(existing.join("keep.txt")).expect("keep"), b"keep");
        assert_eq!(fs::read_dir(&root).expect("root").count(), 1);
    }

    #[test]
    fn rejects_missing_ciphertext_and_leaves_no_partial_result() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        let imported = import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        fs::remove_file(ciphertext_path(&backup_dir, &imported[0].id)).expect("remove cipher");
        let root = target_root(&temp);

        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("missing ciphertext");
        assert_eq!(error.code, "RESTORE_INCOMPLETE");
        assert_eq!(fs::read_dir(&root).expect("root").count(), 0);

        // 失败不影响当前保险库。
        let original = Index::load(&vault, &key).expect("original index");
        assert_eq!(original.entries.len(), 1);
    }

    #[test]
    fn rejects_unreadable_or_corrupt_index() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        let index_path = backup_dir.join(file_store::INDEX_FILE);
        let mut bytes = fs::read(&index_path).expect("index");
        bytes[0] ^= 0xff;
        fs::write(&index_path, bytes).expect("corrupt");

        let root = target_root(&temp);
        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("corrupt index");
        assert_eq!(error.code, "RESTORE_INDEX_INVALID");
        assert_eq!(fs::read_dir(&root).expect("root").count(), 0);
    }

    #[test]
    fn restores_backup_of_another_vault_with_its_own_password() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        let imported = import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);

        // 另一个保险库 B：模拟“当前打开的保险库与备份无关”。
        let parent_b = temp.path().join("vault-b-parent");
        fs::create_dir(&parent_b).expect("parent b");
        let (vault_b, _key_b) =
            create_vault_dir(&parent_b, "another password 456").expect("create vault b");
        let root = target_root(&temp);
        let backup_snapshot = snapshot(&backup_dir);

        let summary = restore_backup(
            &backup_dir,
            &root,
            Some(vault_b.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect("restore other vault's backup");
        let restored = PathBuf::from(&summary.destination);

        // 恢复结果使用备份自己的密码解锁，与保险库 B 无关。
        unlock_vault_dir(&restored, PASSWORD).expect("restored unlocks");
        let restored_index = Index::load(&restored, &key).expect("restored index");
        assert_eq!(restored_index.entries.len(), 1);
        assert_eq!(restored_index.entries[0].id, imported[0].id);
        assert_eq!(
            fs::read(ciphertext_path(&backup_dir, &imported[0].id)).expect("backup cipher"),
            fs::read(ciphertext_path(&restored, &imported[0].id)).expect("restored cipher")
        );

        // 保险库 B 与原备份都不受影响。
        assert!(vault::metadata_path(&vault_b).exists());
        assert_eq!(snapshot(&backup_dir), backup_snapshot);
    }

    #[test]
    fn restores_without_any_current_vault() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        let root = target_root(&temp);

        // 没有当前保险库（如刚启动的应用）也能恢复。
        let summary = restore_backup(&backup_dir, &root, None, PASSWORD, &|| false, &no_progress)
            .expect("restore without current vault");
        let restored = PathBuf::from(&summary.destination);
        unlock_vault_dir(&restored, PASSWORD).expect("restored unlocks");
        assert_eq!(fs::read_dir(&root).expect("root").count(), 1);
    }

    #[test]
    fn rejects_wrong_password_without_creating_any_directory() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        let backup_snapshot = snapshot(&backup_dir);
        let root = target_root(&temp);

        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            "wrong password 999",
            &|| false,
            &no_progress,
        )
        .expect_err("wrong password");
        assert_eq!(error.code, "RESTORE_PASSWORD_INVALID");

        // 密码错误时不留下临时目录或最终目录；原备份保持不变。
        assert_eq!(fs::read_dir(&root).expect("root").count(), 0);
        assert_eq!(snapshot(&backup_dir), backup_snapshot);

        // 正确密码仍然可以恢复。
        restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect("correct password restores");
        assert_eq!(fs::read_dir(&root).expect("root").count(), 1);
    }

    #[test]
    fn verifies_backup_password_independently() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        let backup_snapshot = snapshot(&backup_dir);

        verify_backup_password(&backup_dir, PASSWORD).expect("correct password verifies");
        let wrong = verify_backup_password(&backup_dir, "wrong password 999")
            .expect_err("wrong password must fail");
        assert_eq!(wrong.code, "RESTORE_PASSWORD_INVALID");

        // 不是备份的目录无法通过验证；验证不修改备份内容。
        let empty = temp.path().join("empty-backup");
        fs::create_dir(&empty).expect("empty");
        let error = verify_backup_password(&empty, PASSWORD).expect_err("not a backup");
        assert_eq!(error.code, "RESTORE_INCOMPLETE");
        assert_eq!(snapshot(&backup_dir), backup_snapshot);
    }

    #[test]
    fn rejects_unexpected_or_illegal_files_in_backup() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        let imported = import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        let root = target_root(&temp);

        // 未在索引登记的额外密文文件。
        let bucket = ciphertext_path(&backup_dir, &imported[0].id)
            .parent()
            .expect("bucket")
            .to_path_buf();
        fs::write(bucket.join(format!("{}.hvn", "ab".repeat(16))), b"extra").expect("extra");
        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("extra file");
        assert_eq!(error.code, "RESTORE_INCOMPLETE");
        fs::remove_file(bucket.join(format!("{}.hvn", "ab".repeat(16)))).expect("cleanup extra");

        // 非法文件名。
        fs::write(bucket.join("not-a-ciphertext"), b"x").expect("illegal");
        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("illegal name");
        assert_eq!(error.code, "RESTORE_INCOMPLETE");
        fs::remove_file(bucket.join("not-a-ciphertext")).expect("cleanup illegal");

        assert_eq!(fs::read_dir(&root).expect("root").count(), 0);
    }

    #[test]
    fn rejects_symlinked_ciphertext() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        let imported = import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        let cipher = ciphertext_path(&backup_dir, &imported[0].id);
        let elsewhere = backup_dir.join("elsewhere.hvn");
        fs::rename(&cipher, &elsewhere).expect("move");
        symlink(&elsewhere, &cipher).expect("symlink");

        let root = target_root(&temp);
        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("symlink");
        assert_eq!(error.code, "RESTORE_INCOMPLETE");
        assert_eq!(fs::read_dir(&root).expect("root").count(), 0);
    }

    #[test]
    fn cancelled_restore_cleans_temporary_directory() {
        let (temp, vault, key) = setup();
        let sources = temp.path().join("sources");
        let first = write_source(&sources, "a.txt", b"alpha");
        let second = write_source(&sources, "b.txt", b"beta");
        import(&vault, &key, &[first, second]);
        let backup_dir = make_backup(&temp, &vault, &key);
        let root = target_root(&temp);

        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| true,
            &no_progress,
        )
        .expect_err("cancelled");
        assert_eq!(error.code, "RESTORE_CANCELLED");
        assert_eq!(fs::read_dir(&root).expect("root").count(), 0);
    }

    #[test]
    fn rejects_empty_or_multiple_invalid_backups() {
        let (temp, vault, key) = setup();
        let root = target_root(&temp);

        // 空目录不是有效备份。
        let empty = temp.path().join("empty-backup");
        fs::create_dir(&empty).expect("empty");
        let error = restore_backup(
            &empty,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("empty");
        assert_eq!(error.code, "RESTORE_INCOMPLETE");

        // 缺少索引。
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        fs::remove_file(backup_dir.join(file_store::INDEX_FILE)).expect("remove index");
        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("missing index");
        assert_eq!(error.code, "RESTORE_INCOMPLETE");

        assert_eq!(fs::read_dir(&root).expect("root").count(), 0);
    }

    #[test]
    fn rejects_target_inside_backup_or_current_vault() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);

        // 目标父目录不能位于备份内。
        let inside_backup = backup_dir.join("restore-here");
        fs::create_dir(&inside_backup).expect("inside backup");
        let error = restore_backup(
            &backup_dir,
            &inside_backup,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("inside backup");
        assert_eq!(error.code, "RESTORE_TARGET_INVALID");

        // 目标父目录不能是当前保险库自身。
        let error = restore_backup(
            &backup_dir,
            &vault,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("inside vault");
        assert_eq!(error.code, "RESTORE_TARGET_INVALID");

        // 备份源不能是当前保险库本身。
        let error = restore_backup(
            &vault,
            &target_root(&temp),
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("live vault as source");
        assert_eq!(error.code, "RESTORE_SOURCE_INVALID");
    }

    #[test]
    fn rejects_ciphertext_header_mismatch() {
        let (temp, vault, key) = setup();
        let source = write_source(&temp.path().join("sources"), "a.txt", b"alpha");
        let imported = import(&vault, &key, &[source]);
        let backup_dir = make_backup(&temp, &vault, &key);
        let cipher = ciphertext_path(&backup_dir, &imported[0].id);

        // 只破坏头部中的原始长度字段，文件长度保持不变，仍应被发现。
        let mut bytes = fs::read(&cipher).expect("cipher");
        bytes[43] ^= 0xff;
        fs::write(&cipher, &bytes).expect("tamper");

        let root = target_root(&temp);
        let error = restore_backup(
            &backup_dir,
            &root,
            Some(vault.as_path()),
            PASSWORD,
            &|| false,
            &no_progress,
        )
        .expect_err("header mismatch");
        assert_eq!(error.code, "RESTORE_INCOMPLETE");
        assert_eq!(fs::read_dir(&root).expect("root").count(), 0);
    }
}
