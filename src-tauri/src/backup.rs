//! FR-44 手动备份：在共享锁下原样复制保险库必需的加密数据。
//!
//! 安全边界：
//! - 只复制 `vault.json`、`index.hvn` 与索引中登记的文件密文，不复制锁文件、
//!   `tmp/` 临时目录或其它运行时缓存。
//! - 不读取文件明文、不解密文件密文；索引只在内存中解密用于校验完整性。
//! - 整个复制过程持有库级共享锁，所有写操作（导入、删除、重命名、移动、收藏、
//!   文件夹变更）需要排他锁，因此备份期间无法改变保险库数据。
//! - 失败或取消时删除本次创建的备份目录，不覆盖已有备份。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use serde::Serialize;

use crate::file_store::{self, Index, VaultLock};
use crate::vault::{self, VaultError, unix_now};

const COPY_BUFFER_BYTES: usize = 64 * 1024;
const BACKUP_DIR_PREFIX: &str = "haven-backup-";

/// 备份结果：只包含目标位置、密文数量与总字节数，不包含任何文件名或路径细节。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSummary {
    pub destination: String,
    pub file_count: u64,
    pub total_bytes: u64,
}

/// 备份进度：仅在复制文件密文阶段按文件推进。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupProgress {
    pub processed: u64,
    pub total: u64,
    pub bytes: u64,
}

fn backup_busy() -> VaultError {
    VaultError::new("BACKUP_BUSY", "其他文件操作尚未结束，请稍后重试备份。")
}

fn backup_target_invalid() -> VaultError {
    VaultError::new("BACKUP_TARGET_INVALID", "备份位置不可用，请选择其他目录。")
}

fn backup_target_exists() -> VaultError {
    VaultError::new(
        "BACKUP_TARGET_EXISTS",
        "目标位置已存在同名备份，请选择其他目录。",
    )
}

fn backup_incomplete() -> VaultError {
    VaultError::new(
        "BACKUP_INCOMPLETE",
        "保险库数据不完整，已取消备份并清理不完整结果。",
    )
}

fn backup_cancelled() -> VaultError {
    VaultError::new("BACKUP_CANCELLED", "备份已取消，不完整结果已清理。")
}

fn backup_failed() -> VaultError {
    VaultError::new(
        "BACKUP_FAILED",
        "备份失败，已取消本次备份并清理不完整结果。",
    )
}

/// 备份插件入口：使用当前时间生成唯一备份目录。
pub fn backup_vault(
    vault_dir: &Path,
    master_key: &[u8; 32],
    target_root: &Path,
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(BackupProgress),
) -> Result<BackupSummary, VaultError> {
    backup_vault_at(
        vault_dir,
        master_key,
        target_root,
        unix_now()?,
        is_cancelled,
        progress,
    )
}

fn backup_vault_at(
    vault_dir: &Path,
    master_key: &[u8; 32],
    target_root: &Path,
    timestamp: u64,
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(BackupProgress),
) -> Result<BackupSummary, VaultError> {
    if !target_root.is_absolute() || !target_root.is_dir() {
        return Err(backup_target_invalid());
    }
    let vault_canonical = fs::canonicalize(vault_dir).map_err(|_| backup_target_invalid())?;
    let target_canonical = fs::canonicalize(target_root).map_err(|_| backup_target_invalid())?;
    if target_canonical.starts_with(&vault_canonical) {
        // 禁止把备份写进保险库自身，避免污染源数据或形成递归复制。
        return Err(backup_target_invalid());
    }

    // 共享锁覆盖整个“读取索引 + 复制密文”过程；写操作需要排他锁，会被拒绝。
    let _vault_lock = VaultLock::acquire_shared(vault_dir).map_err(|error| {
        if error.code == "VAULT_BUSY" {
            backup_busy()
        } else {
            error
        }
    })?;

    let index = Index::load(vault_dir, master_key)?;
    let metadata_path = vault::metadata_path(vault_dir);
    let index_path = vault_dir.join(file_store::INDEX_FILE);
    // 元数据与索引是恢复所必需的文件；缺失或非普通文件时拒绝备份。
    regular_file_size(&metadata_path)?;
    regular_file_size(&index_path)?;

    let mut sources = Vec::with_capacity(index.entries.len());
    for entry in &index.entries {
        let path = file_store::ciphertext_path(vault_dir, &entry.id);
        // 先确认每个登记文件都是普通文件且存在，缺失即判定备份不完整。
        regular_file_size(&path)?;
        sources.push((entry.id.clone(), path));
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));

    let destination = target_root.join(format!("{BACKUP_DIR_PREFIX}{timestamp}"));
    match create_private_dir(&destination) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            return Err(backup_target_exists());
        }
        Err(_) => return Err(backup_target_invalid()),
    }

    let total_files = sources.len() as u64;
    let mut processed = 0u64;
    let mut copied_bytes = 0u64;

    let outcome = (|| -> Result<(), VaultError> {
        ensure_private_dir(&destination.join(file_store::FILES_DIR))
            .map_err(|_| backup_failed())?;

        copied_bytes += copy_regular_file(&metadata_path, &destination.join("vault.json"))?;
        copied_bytes += copy_regular_file(&index_path, &destination.join(file_store::INDEX_FILE))?;
        progress(BackupProgress {
            processed,
            total: total_files,
            bytes: copied_bytes,
        });

        for (_id, source) in &sources {
            if is_cancelled() {
                return Err(backup_cancelled());
            }
            let relative = source
                .strip_prefix(vault_dir)
                .map_err(|_| backup_incomplete())?;
            let target = destination.join(relative);
            if let Some(parent) = target.parent() {
                ensure_private_dir(parent).map_err(|_| backup_failed())?;
            }
            copied_bytes += copy_regular_file(source, &target)?;
            processed += 1;
            progress(BackupProgress {
                processed,
                total: total_files,
                bytes: copied_bytes,
            });
        }

        sync_dir(&destination);
        Ok(())
    })();

    match outcome {
        Ok(()) => Ok(BackupSummary {
            destination: destination.display().to_string(),
            file_count: total_files,
            total_bytes: copied_bytes,
        }),
        Err(error) => {
            // 本次新建的目录整体删除；原保险库不会受到影响。
            let _ = fs::remove_dir_all(&destination);
            Err(error)
        }
    }
}

fn regular_file_size(path: &Path) -> Result<u64, VaultError> {
    let info = fs::symlink_metadata(path).map_err(|_| backup_incomplete())?;
    if info.file_type().is_symlink() || !info.is_file() {
        return Err(backup_incomplete());
    }
    Ok(info.len())
}

pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// 已存在且为普通目录时复用，否则创建；拒绝把目录写到符号链接后面。
pub(crate) fn ensure_private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(info) if info.is_dir() && !info.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidInput, "not a dir")),
        Err(err) if err.kind() == io::ErrorKind::NotFound => create_private_dir(path),
        Err(err) => Err(err),
    }
}

/// 只复制普通文件，拒绝符号链接与特殊文件；目标必须以 create_new 打开。
fn copy_regular_file(source: &Path, target: &Path) -> Result<u64, VaultError> {
    let mut input = open_regular_file(source).map_err(|_| backup_incomplete())?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(target).map_err(|_| backup_failed())?;

    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        let read = input.read(&mut buffer).map_err(|_| backup_failed())?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|_| backup_failed())?;
        total += read as u64;
    }
    output.sync_all().map_err(|_| backup_failed())?;
    Ok(total)
}

pub(crate) fn open_regular_file(path: &Path) -> io::Result<File> {
    let info = fs::symlink_metadata(path)?;
    if info.file_type().is_symlink() || !info.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

pub(crate) fn sync_dir(path: &Path) {
    if let Ok(handle) = File::open(path) {
        let _ = handle.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_store::{
        ImportedFile, create_folder, import_batch, list_folders, move_files, prepare_vault,
        set_favorite,
    };
    use crate::vault::create_vault_dir;
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

    fn no_progress(_: BackupProgress) {}

    #[test]
    fn backup_copies_index_and_ciphertexts_and_source_stays_usable() {
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

        let target = temp.path().join("backup-target");
        fs::create_dir(&target).expect("target");

        let summary = backup_vault_at(
            &vault,
            &key,
            &target,
            1_700_000_000,
            &|| false,
            &no_progress,
        )
        .expect("backup");

        assert_eq!(summary.file_count, 2);
        assert!(summary.total_bytes > 0);
        let destination = PathBuf::from(&summary.destination);
        assert!(destination.starts_with(&target));
        assert!(destination.join("vault.json").is_file());
        assert!(destination.join(file_store::INDEX_FILE).is_file());

        let backup_index = Index::load(&destination, &key).expect("backup index loads");
        assert_eq!(backup_index.entries.len(), 2);
        assert_eq!(backup_index.folders.len(), 1);
        assert_eq!(backup_index.folders[0].name, "资料");
        let favorite_entry = backup_index
            .entries
            .iter()
            .find(|entry| entry.id == imported[0].id)
            .expect("favorite entry");
        assert!(favorite_entry.favorite);
        assert_eq!(
            favorite_entry.folder_id.as_deref(),
            Some(folder.id.as_str())
        );

        // 每个密文逐字节一致，且备份目录不包含锁文件或 tmp 临时目录。
        for entry in &backup_index.entries {
            let source = file_store::ciphertext_path(&vault, &entry.id);
            let copied = file_store::ciphertext_path(&destination, &entry.id);
            assert_eq!(
                fs::read(&source).expect("source"),
                fs::read(&copied).expect("copy")
            );
        }
        assert!(!destination.join(".haven.lock").exists());
        assert!(!destination.join(file_store::TMP_DIR).exists());

        // 原保险库继续可用：索引可读、文件夹与收藏未变。
        let original = Index::load(&vault, &key).expect("original index");
        assert_eq!(original.entries.len(), 2);
        assert_eq!(list_folders(&vault, &key).expect("folders").len(), 1);
        assert!(original.entries.iter().any(|entry| entry.favorite));
        assert!(
            original
                .entries
                .iter()
                .any(|entry| entry.folder_id.as_deref() == Some(folder.id.as_str()))
        );
    }

    #[test]
    fn backup_refuses_existing_destination_without_touching_it() {
        let (temp, vault, key) = setup();
        let target = temp.path().join("backup-target");
        fs::create_dir(&target).expect("target");
        let existing = target.join(format!("{BACKUP_DIR_PREFIX}1700000000"));
        fs::create_dir(&existing).expect("existing");
        fs::write(existing.join("keep.txt"), b"keep").expect("keep");

        let error = backup_vault_at(
            &vault,
            &key,
            &target,
            1_700_000_000,
            &|| false,
            &no_progress,
        )
        .expect_err("must refuse");
        assert_eq!(error.code, "BACKUP_TARGET_EXISTS");
        assert_eq!(fs::read(existing.join("keep.txt")).expect("keep"), b"keep");
    }

    #[test]
    fn backup_rejects_target_inside_vault() {
        let (_temp, vault, key) = setup();
        let inside = vault.join("backup-here");
        fs::create_dir(&inside).expect("inside");

        let error = backup_vault_at(
            &vault,
            &key,
            &inside,
            1_700_000_000,
            &|| false,
            &no_progress,
        )
        .expect_err("inside vault");
        assert_eq!(error.code, "BACKUP_TARGET_INVALID");
    }

    #[test]
    fn backup_blocks_index_writes_while_running() {
        let (temp, vault, key) = setup();
        let sources = temp.path().join("sources");
        let source = write_source(&sources, "a.txt", b"alpha");
        let imported = import(&vault, &key, &[source]);
        let target = temp.path().join("backup-target");
        fs::create_dir(&target).expect("target");

        let observed = std::cell::RefCell::new(Vec::new());
        backup_vault_at(
            &vault,
            &key,
            &target,
            1_700_000_001,
            &|| false,
            &|_progress| {
                if observed.borrow().is_empty() {
                    let error = set_favorite(&vault, &key, &imported[0].id, true)
                        .expect_err("write must be blocked");
                    observed.borrow_mut().push(error.code);
                }
            },
        )
        .expect("backup");

        assert_eq!(observed.borrow()[0], "VAULT_BUSY");
        let index = Index::load(&vault, &key).expect("index");
        assert!(!index.entries[0].favorite);
    }

    #[test]
    fn cancelled_backup_cleans_partial_result() {
        let (temp, vault, key) = setup();
        let sources = temp.path().join("sources");
        let first = write_source(&sources, "a.txt", b"alpha");
        let second = write_source(&sources, "b.txt", b"beta");
        import(&vault, &key, &[first, second]);

        let target = temp.path().join("backup-target");
        fs::create_dir(&target).expect("target");

        let error = backup_vault_at(&vault, &key, &target, 1_700_000_000, &|| true, &no_progress)
            .expect_err("cancelled");
        assert_eq!(error.code, "BACKUP_CANCELLED");
        assert!(
            !target
                .join(format!("{BACKUP_DIR_PREFIX}1700000000"))
                .exists()
        );
        assert_eq!(fs::read_dir(&target).expect("target").count(), 0);
    }

    #[test]
    fn incomplete_source_fails_and_cleans_partial_result() {
        let (temp, vault, key) = setup();
        let sources = temp.path().join("sources");
        let first = write_source(&sources, "a.txt", b"alpha");
        let imported = import(&vault, &key, &[first]);
        let entry = &imported[0];
        fs::remove_file(file_store::ciphertext_path(&vault, &entry.id)).expect("remove source");

        let target = temp.path().join("backup-target");
        fs::create_dir(&target).expect("target");
        let error = backup_vault_at(
            &vault,
            &key,
            &target,
            1_700_000_000,
            &|| false,
            &no_progress,
        )
        .expect_err("incomplete");
        assert_eq!(error.code, "BACKUP_INCOMPLETE");
        assert_eq!(fs::read_dir(&target).expect("target").count(), 0);
    }
}
