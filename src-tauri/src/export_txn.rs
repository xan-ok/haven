//! 导出事务：sidecar 活动锁、临时文件、原子提交与持久化同步。
//!
//! 协议不变量：
//! - sidecar 先于 temp 创建、加锁并持久化；temp 任何时刻都对应一个已持锁的 sidecar。
//! - 提交使用已校验目录 fd 上的 `renameatx_np(RENAME_EXCL)`，保持同卷原子且绝不覆盖。
//! - rename 成功后绝不删除最终文件；目录同步失败时保留 sidecar，由恢复扫描清理。
//! - 提交结果不确定时保留 temp 与 sidecar，返回 `EXPORT_COMMIT_UNKNOWN`。

use std::ffi::{CString, OsStr, OsString, c_char, c_int, c_uint};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;

use crate::export_staging::{
    self, ExportTarget, SIDECAR_SUFFIX, StagingDir, TEMP_PREFIX, TEMP_SUFFIX, createat_private,
    entry_presence, fstat, fsync_dir, recovery_guard, register_active, same_volume, unlinkat,
    unregister_active,
};
use crate::vault::{VaultError, random_suffix};

const RENAME_EXCL: c_uint = 0x0000_0004;

/// 仅测试构建启用的可控故障注入。生产构建中不存在任何触发入口。
#[cfg(test)]
pub(crate) mod fault_injection {
    use std::cell::Cell;

    thread_local! {
        static TARGET_SYNC_FAILURE: Cell<bool> = const { Cell::new(false) };
        static STAGING_SYNC_FAILURE: Cell<bool> = const { Cell::new(false) };
        static TEMP_SYNC_FAILURE: Cell<bool> = const { Cell::new(false) };
        static RENAME_UNKNOWN: Cell<bool> = const { Cell::new(false) };
        static RENAME_EXDEV: Cell<bool> = const { Cell::new(false) };
        static TEMP_UNLINK_FAILURE: Cell<bool> = const { Cell::new(false) };
        static SIDECAR_UNLINK_FAILURE: Cell<bool> = const { Cell::new(false) };
        static BEGIN_DIR_SYNC_FAILURE: Cell<bool> = const { Cell::new(false) };
        static RENAME_ENOENT: Cell<bool> = const { Cell::new(false) };
        static TEMP_STAT_FAILURE: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn set_target_sync_failure(enabled: bool) {
        TARGET_SYNC_FAILURE.with(|flag| flag.set(enabled));
    }

    pub(crate) fn target_sync_failure() -> bool {
        TARGET_SYNC_FAILURE.with(|flag| flag.get())
    }

    pub(crate) fn set_staging_sync_failure(enabled: bool) {
        STAGING_SYNC_FAILURE.with(|flag| flag.set(enabled));
    }

    pub(crate) fn staging_sync_failure() -> bool {
        STAGING_SYNC_FAILURE.with(|flag| flag.get())
    }

    pub(crate) fn set_temp_sync_failure(enabled: bool) {
        TEMP_SYNC_FAILURE.with(|flag| flag.set(enabled));
    }

    pub(crate) fn temp_sync_failure() -> bool {
        TEMP_SYNC_FAILURE.with(|flag| flag.get())
    }

    pub(crate) fn set_rename_unknown(enabled: bool) {
        RENAME_UNKNOWN.with(|flag| flag.set(enabled));
    }

    pub(crate) fn rename_unknown() -> bool {
        RENAME_UNKNOWN.with(|flag| flag.get())
    }

    pub(crate) fn set_rename_exdev(enabled: bool) {
        RENAME_EXDEV.with(|flag| flag.set(enabled));
    }

    pub(crate) fn rename_exdev() -> bool {
        RENAME_EXDEV.with(|flag| flag.get())
    }

    pub(crate) fn set_temp_unlink_failure(enabled: bool) {
        TEMP_UNLINK_FAILURE.with(|flag| flag.set(enabled));
    }

    pub(crate) fn temp_unlink_failure() -> bool {
        TEMP_UNLINK_FAILURE.with(|flag| flag.get())
    }

    pub(crate) fn set_sidecar_unlink_failure(enabled: bool) {
        SIDECAR_UNLINK_FAILURE.with(|flag| flag.set(enabled));
    }

    pub(crate) fn sidecar_unlink_failure() -> bool {
        SIDECAR_UNLINK_FAILURE.with(|flag| flag.get())
    }

    pub(crate) fn set_begin_dir_sync_failure(enabled: bool) {
        BEGIN_DIR_SYNC_FAILURE.with(|flag| flag.set(enabled));
    }

    pub(crate) fn begin_dir_sync_failure() -> bool {
        BEGIN_DIR_SYNC_FAILURE.with(|flag| flag.get())
    }

    pub(crate) fn set_rename_enoent(enabled: bool) {
        RENAME_ENOENT.with(|flag| flag.set(enabled));
    }

    pub(crate) fn rename_enoent() -> bool {
        RENAME_ENOENT.with(|flag| flag.get())
    }

    pub(crate) fn set_temp_stat_failure(enabled: bool) {
        TEMP_STAT_FAILURE.with(|flag| flag.set(enabled));
    }

    pub(crate) fn temp_stat_failure() -> bool {
        TEMP_STAT_FAILURE.with(|flag| flag.get())
    }
}

unsafe extern "C" {
    fn renameatx_np(
        fromfd: c_int,
        from: *const c_char,
        tofd: c_int,
        to: *const c_char,
        flags: c_uint,
    ) -> c_int;
}

/// 提交结果分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportCommit {
    /// 确定没有提交，可以清理 temp/sidecar。
    NotCommitted,
    /// 确定已提交，最终文件存在。
    Committed,
    /// 无法确定；调用方必须保留现场。
    Unknown,
}

/// 提交结果。`durability_confirmed=false` 表示 rename 已成功但目录同步未确认。
#[derive(Debug)]
pub struct CommitResult {
    pub commit: ExportCommit,
    pub durability_confirmed: bool,
    pub error: Option<VaultError>,
}

fn stage_error() -> VaultError {
    VaultError::export_staging_unavailable()
}

fn map_create_error(error: &io::Error) -> VaultError {
    if error.kind() == io::ErrorKind::StorageFull || error.raw_os_error() == Some(28) {
        VaultError::no_space()
    } else {
        VaultError::export_write_failed()
    }
}

/// 放弃未提交事务时的清理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupOutcome {
    /// temp 已删除或本来不存在，sidecar 已删除。
    Clean,
    /// temp 或 sidecar 未能安全清理；sidecar 保留给恢复流程。
    SidecarRetained,
}

fn unlink_temp(dir_fd: RawFd, name: &OsStr) -> io::Result<()> {
    #[cfg(test)]
    if fault_injection::temp_unlink_failure() {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    unlinkat(dir_fd, name)
}

fn unlink_sidecar(dir_fd: RawFd, name: &OsStr) -> io::Result<()> {
    #[cfg(test)]
    if fault_injection::sidecar_unlink_failure() {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    unlinkat(dir_fd, name)
}

/// 只有确认 temp 已删除或本来不存在时，才允许删除 sidecar。
/// 任一步失败都保留 sidecar，使后续恢复仍能通过 sidecar 找到残留明文。
fn cleanup_uncommitted(
    staging_fd: &File,
    temp_name: &OsStr,
    sidecar_name: &OsStr,
) -> CleanupOutcome {
    match unlink_temp(staging_fd.as_raw_fd(), temp_name) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return CleanupOutcome::SidecarRetained,
    }
    // temp 删除未持久化时，崩溃后仍可能重新出现；此时保留 sidecar。
    if fsync_dir(staging_fd).is_err() {
        return CleanupOutcome::SidecarRetained;
    }
    if unlink_sidecar(staging_fd.as_raw_fd(), sidecar_name).is_err() {
        return CleanupOutcome::SidecarRetained;
    }
    let _ = fsync_dir(staging_fd);
    CleanupOutcome::Clean
}

/// 清理结果合并进错误：清理未完成时优先报告专用错误，否则保留原始错误分类。
fn cleanup_uncommitted_or_error(
    staging_fd: &File,
    temp_name: &OsStr,
    sidecar_name: &OsStr,
    error: VaultError,
) -> VaultError {
    match cleanup_uncommitted(staging_fd, temp_name, sidecar_name) {
        CleanupOutcome::Clean => error,
        CleanupOutcome::SidecarRetained => VaultError::export_cleanup_pending(),
    }
}

fn cstring(name: &OsStr) -> Result<CString, VaultError> {
    CString::new(name.as_bytes()).map_err(|_| VaultError::internal())
}

/// 一次导出的临时文件事务。
pub struct ExportTxn {
    staging_fd: File,
    /// 持有 sidecar fd 即持有活动锁；字段本身只用于维持锁的生命周期。
    _sidecar_fd: File,
    temp_fd: Option<File>,
    temp_name: OsString,
    sidecar_name: OsString,
    token: String,
    dev: u64,
    ino: u64,
    finished: bool,
    keep_scene: bool,
}

impl ExportTxn {
    /// 创建 sidecar（加锁并持久化）后创建 0600 临时文件。
    pub fn begin(staging: &StagingDir) -> Result<Self, VaultError> {
        let _guard = recovery_guard();
        let token = random_suffix()?;
        let staging_fd = staging.dir_fd().try_clone().map_err(|_| stage_error())?;
        let sidecar_name = OsString::from(format!("{TEMP_PREFIX}{token}{SIDECAR_SUFFIX}"));
        let temp_name = OsString::from(format!("{TEMP_PREFIX}{token}{TEMP_SUFFIX}"));

        let sidecar_fd =
            createat_private(staging_fd.as_raw_fd(), &sidecar_name, libc::O_RDWR, 0o600)
                .map_err(|_| stage_error())?;
        // SAFETY: sidecar_fd 在调用期间保持打开；flock 只修改该描述符的锁状态。
        if unsafe { libc::flock(sidecar_fd.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(cleanup_uncommitted_or_error(
                &staging_fd,
                &temp_name,
                &sidecar_name,
                stage_error(),
            ));
        }
        if sidecar_fd.sync_all().is_err() || fsync_dir(&staging_fd).is_err() {
            return Err(cleanup_uncommitted_or_error(
                &staging_fd,
                &temp_name,
                &sidecar_name,
                stage_error(),
            ));
        }

        let temp_fd =
            match createat_private(staging_fd.as_raw_fd(), &temp_name, libc::O_WRONLY, 0o600) {
                Ok(file) => file,
                Err(err) => {
                    return Err(cleanup_uncommitted_or_error(
                        &staging_fd,
                        &temp_name,
                        &sidecar_name,
                        map_create_error(&err),
                    ));
                }
            };
        if unsafe { libc::fchmod(temp_fd.as_raw_fd(), 0o600) } != 0 {
            return Err(cleanup_uncommitted_or_error(
                &staging_fd,
                &temp_name,
                &sidecar_name,
                stage_error(),
            ));
        }
        let info = match fstat(temp_fd.as_raw_fd()) {
            Ok(info) => info,
            Err(_) => {
                return Err(cleanup_uncommitted_or_error(
                    &staging_fd,
                    &temp_name,
                    &sidecar_name,
                    stage_error(),
                ));
            }
        };
        if (info.st_mode as u32) & 0o170000 != 0o100000
            || info.st_uid as u32 != unsafe { libc::geteuid() as u32 }
            || (info.st_mode as u32) & 0o777 != 0o600
        {
            return Err(cleanup_uncommitted_or_error(
                &staging_fd,
                &temp_name,
                &sidecar_name,
                stage_error(),
            ));
        }
        #[cfg(test)]
        let dir_sync_failed =
            fault_injection::begin_dir_sync_failure() || fsync_dir(&staging_fd).is_err();
        #[cfg(not(test))]
        let dir_sync_failed = fsync_dir(&staging_fd).is_err();
        if dir_sync_failed {
            return Err(cleanup_uncommitted_or_error(
                &staging_fd,
                &temp_name,
                &sidecar_name,
                stage_error(),
            ));
        }

        register_active(staging.dev(), staging.ino(), &token);
        Ok(Self {
            staging_fd,
            _sidecar_fd: sidecar_fd,
            temp_fd: Some(temp_fd),
            temp_name,
            sidecar_name,
            token,
            dev: staging.dev(),
            ino: staging.ino(),
            finished: false,
            keep_scene: false,
        })
    }

    /// 临时文件句柄；明文只写入这里。
    pub fn temp_mut(&mut self) -> &mut File {
        self.temp_fd.as_mut().expect("temp fd alive")
    }

    /// 把明文数据持久化到磁盘；失败时调用方必须放弃提交。
    pub fn sync_temp(&mut self) -> Result<(), VaultError> {
        #[cfg(test)]
        if fault_injection::temp_sync_failure() {
            return Err(VaultError::export_write_failed());
        }
        let temp = self.temp_fd.as_ref().ok_or_else(stage_error)?;
        temp.sync_all().map_err(|error| {
            if error.kind() == io::ErrorKind::StorageFull || error.raw_os_error() == Some(28) {
                VaultError::no_space()
            } else {
                VaultError::export_write_failed()
            }
        })
    }

    /// 放弃未提交的事务：先删 temp，确认成功或不存在后才删 sidecar。
    /// 清理不完整时保留 sidecar，交给恢复流程；返回值用于调用方决定是否报告清理未完成。
    pub(crate) fn abort(&mut self) -> CleanupOutcome {
        if self.finished {
            return CleanupOutcome::Clean;
        }
        self.finished = true;
        drop(self.temp_fd.take());
        let outcome = cleanup_uncommitted(&self.staging_fd, &self.temp_name, &self.sidecar_name);
        unregister_active(self.dev, self.ino, &self.token);
        outcome
    }

    /// 未提交失败后的清理；清理不完整时返回专门错误，避免假报「已清理」。
    fn abort_with_error(&mut self, error: VaultError) -> CommitResult {
        not_committed(self.abort_error(error))
    }

    /// 放弃未提交事务并把清理结果合并进错误：清理未完成时优先报告专用错误。
    pub(crate) fn abort_error(&mut self, error: VaultError) -> VaultError {
        match self.abort() {
            CleanupOutcome::Clean => error,
            CleanupOutcome::SidecarRetained => VaultError::export_cleanup_pending(),
        }
    }

    fn cleanup_sidecar_after_commit(&mut self) {
        let _ = unlinkat(self.staging_fd.as_raw_fd(), &self.sidecar_name);
        let _ = fsync_dir(&self.staging_fd);
        self.finished = true;
        unregister_active(self.dev, self.ino, &self.token);
    }

    /// temp 状态：`Ok(true)` 存在，`Ok(false)` 明确不存在，`Err` 无法确认。
    fn temp_presence(&self) -> io::Result<bool> {
        #[cfg(test)]
        if fault_injection::temp_stat_failure() {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        entry_presence(self.staging_fd.as_raw_fd(), &self.temp_name)
    }

    /// 同卷原子提交。进入该函数后不再响应取消。
    pub fn commit(&mut self, staging: &StagingDir, target: &ExportTarget) -> CommitResult {
        let _guard = recovery_guard();
        drop(self.temp_fd.take());

        #[cfg(test)]
        if fault_injection::rename_unknown() {
            self.keep_scene = true;
            self.finished = true;
            unregister_active(self.dev, self.ino, &self.token);
            return CommitResult {
                commit: ExportCommit::Unknown,
                durability_confirmed: false,
                error: Some(VaultError::export_commit_unknown()),
            };
        }

        let staging_now = match export_staging::capture_volume(staging.dir_fd().as_raw_fd()) {
            Ok(identity) => identity,
            Err(error) => return self.abort_with_error(error),
        };
        let target_now = match export_staging::capture_volume(target.dir_fd().as_raw_fd()) {
            Ok(identity) => identity,
            Err(error) => return self.abort_with_error(error),
        };
        if !same_volume(&staging_now, staging.identity())
            || !same_volume(&target_now, target.identity())
            || !same_volume(&staging_now, &target_now)
        {
            return self.abort_with_error(stage_error());
        }

        let from = match cstring(&self.temp_name) {
            Ok(value) => value,
            Err(error) => return self.abort_with_error(error),
        };
        let to = match cstring(target.final_name()) {
            Ok(value) => value,
            Err(error) => return self.abort_with_error(error),
        };
        // SAFETY: 两个目录 fd 与名称在调用期间有效；renameatx_np 不保留引用。
        #[cfg(test)]
        let result = if fault_injection::rename_exdev() || fault_injection::rename_enoent() {
            -1
        } else {
            unsafe {
                renameatx_np(
                    self.staging_fd.as_raw_fd(),
                    from.as_ptr(),
                    target.dir_fd().as_raw_fd(),
                    to.as_ptr(),
                    RENAME_EXCL,
                )
            }
        };
        #[cfg(not(test))]
        let result = unsafe {
            renameatx_np(
                self.staging_fd.as_raw_fd(),
                from.as_ptr(),
                target.dir_fd().as_raw_fd(),
                to.as_ptr(),
                RENAME_EXCL,
            )
        };
        if result == 0 {
            return self.commit_succeeded(target);
        }

        #[cfg(test)]
        let error = if fault_injection::rename_enoent() {
            io::Error::from_raw_os_error(libc::ENOENT)
        } else if fault_injection::rename_exdev() {
            io::Error::from_raw_os_error(libc::EXDEV)
        } else {
            io::Error::last_os_error()
        };
        #[cfg(not(test))]
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EEXIST) => self.abort_with_error(VaultError::file_exists()),
            Some(libc::EXDEV) => self.abort_with_error(stage_error()),
            // 明确的本地失败：目标目录不可写/只读/空间不足，确定没有提交。
            Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::EROFS) | Some(libc::ENOSPC)
            | Some(libc::EDQUOT) => self.abort_with_error(
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSPC) | Some(libc::EDQUOT)
                ) {
                    VaultError::no_space()
                } else {
                    VaultError::export_write_failed()
                },
            ),
            Some(libc::ENOENT) => {
                // 单次 rename 成功必然返回 0；ENOENT 不可能是“已提交”的证据。
                // 目标同名文件无法证明属于本次事务，因此只依据 temp 状态分类：
                // 存在 → 确定未提交；不存在或无法确认 → 保留现场并报告结果未确认。
                match self.temp_presence() {
                    Ok(true) => self.abort_with_error(VaultError::export_write_failed()),
                    Ok(false) | Err(_) => {
                        self.keep_scene = true;
                        self.finished = true;
                        unregister_active(self.dev, self.ino, &self.token);
                        CommitResult {
                            commit: ExportCommit::Unknown,
                            durability_confirmed: false,
                            error: Some(VaultError::export_commit_unknown()),
                        }
                    }
                }
            }
            _ => {
                self.keep_scene = true;
                self.finished = true;
                unregister_active(self.dev, self.ino, &self.token);
                CommitResult {
                    commit: ExportCommit::Unknown,
                    durability_confirmed: false,
                    error: Some(VaultError::export_commit_unknown()),
                }
            }
        }
    }

    fn commit_succeeded(&mut self, target: &ExportTarget) -> CommitResult {
        #[cfg(test)]
        let target_synced =
            !fault_injection::target_sync_failure() && fsync_dir(target.dir_fd()).is_ok();
        #[cfg(not(test))]
        let target_synced = fsync_dir(target.dir_fd()).is_ok();
        #[cfg(test)]
        let staging_synced =
            !fault_injection::staging_sync_failure() && fsync_dir(&self.staging_fd).is_ok();
        #[cfg(not(test))]
        let staging_synced = fsync_dir(&self.staging_fd).is_ok();
        let durability = target_synced && staging_synced;
        if durability {
            // 只有两个目录都持久化后才删除 sidecar。
            self.cleanup_sidecar_after_commit();
        } else {
            // 保留 sidecar 与最终文件，交给恢复扫描；绝不删除最终文件。
            self.finished = true;
            unregister_active(self.dev, self.ino, &self.token);
        }
        CommitResult {
            commit: ExportCommit::Committed,
            durability_confirmed: durability,
            error: None,
        }
    }
}

impl Drop for ExportTxn {
    fn drop(&mut self) {
        if !self.finished {
            // Drop 无法向用户回报；清理不完整时 sidecar 已保留，由启动恢复继续处理。
            self.abort();
        } else if !self.keep_scene {
            // 已提交且现场已清理；无需额外动作。
        }
    }
}

fn not_committed(error: VaultError) -> CommitResult {
    CommitResult {
        commit: ExportCommit::NotCommitted,
        durability_confirmed: true,
        error: Some(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export_staging::{ExportTarget, STAGING_DIR_NAME, StagingDir};
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use tempfile::TempDir;

    struct Fixture {
        temp: TempDir,
        cache: std::path::PathBuf,
        out: std::path::PathBuf,
        target: ExportTarget,
        staging: StagingDir,
    }

    fn fixture() -> Fixture {
        let temp = TempDir::new().expect("temp");
        let cache = temp.path().join("cache");
        let out = temp.path().join("out");
        std::fs::create_dir(&cache).expect("cache");
        std::fs::create_dir(&out).expect("out");
        let target = ExportTarget::open(&temp.path().join("vault"), &out.join("final.txt"), &[])
            .expect("target");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        Fixture {
            temp,
            cache,
            out,
            target,
            staging,
        }
    }

    fn stage_names(fixture: &Fixture) -> Vec<String> {
        std::fs::read_dir(fixture.cache.join(STAGING_DIR_NAME))
            .expect("read staging")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn export_names(fixture: &Fixture) -> Vec<String> {
        let names = stage_names(fixture);
        names
            .into_iter()
            .filter(|name| name.starts_with(TEMP_PREFIX))
            .collect()
    }

    #[test]
    fn begin_creates_private_sidecar_and_temp() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"secret").expect("write");

        let names = export_names(&fixture);
        assert_eq!(names.len(), 2, "sidecar + temp");
        for name in names {
            let path = fixture.cache.join(STAGING_DIR_NAME).join(&name);
            let metadata = std::fs::metadata(&path).expect("meta");
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        }
        txn.abort();
        assert!(export_names(&fixture).is_empty());
    }

    #[test]
    fn begin_failure_keeps_sidecar_when_temp_unlink_fails() {
        let fixture = fixture();
        fault_injection::set_begin_dir_sync_failure(true);
        fault_injection::set_temp_unlink_failure(true);
        let error = match ExportTxn::begin(&fixture.staging) {
            Ok(_) => panic!("begin must fail"),
            Err(error) => error,
        };
        fault_injection::set_begin_dir_sync_failure(false);
        fault_injection::set_temp_unlink_failure(false);

        assert_eq!(error.code, "EXPORT_CLEANUP_PENDING");
        let names = export_names(&fixture);
        assert_eq!(names.len(), 2, "temp 与 sidecar 都必须保留");
        assert!(names.iter().any(|name| name.ends_with(TEMP_SUFFIX)));
        assert!(names.iter().any(|name| name.ends_with(SIDECAR_SUFFIX)));
        assert!(!fixture.temp.path().join("out/final.txt").exists());
    }

    #[test]
    fn begin_failure_reports_cleanup_pending_when_sidecar_unlink_fails() {
        let fixture = fixture();
        fault_injection::set_begin_dir_sync_failure(true);
        fault_injection::set_sidecar_unlink_failure(true);
        let error = match ExportTxn::begin(&fixture.staging) {
            Ok(_) => panic!("begin must fail"),
            Err(error) => error,
        };
        fault_injection::set_begin_dir_sync_failure(false);
        fault_injection::set_sidecar_unlink_failure(false);

        assert_eq!(error.code, "EXPORT_CLEANUP_PENDING");
        let names = export_names(&fixture);
        assert_eq!(names.len(), 1, "temp 已删除，sidecar 保留");
        assert!(
            names[0].ends_with(SIDECAR_SUFFIX),
            "剩余条目必须是 sidecar，而不是 temp"
        );
        assert!(!fixture.temp.path().join("out/final.txt").exists());
    }

    #[test]
    fn begin_failure_preserves_original_error_when_cleanup_succeeds() {
        let fixture = fixture();
        fault_injection::set_begin_dir_sync_failure(true);
        let error = match ExportTxn::begin(&fixture.staging) {
            Ok(_) => panic!("begin must fail"),
            Err(error) => error,
        };
        fault_injection::set_begin_dir_sync_failure(false);

        assert_eq!(error.code, "EXPORT_STAGING_UNAVAILABLE");
        assert!(export_names(&fixture).is_empty());
        assert!(!fixture.temp.path().join("out/final.txt").exists());
    }

    #[test]
    fn abort_removes_temp_and_sidecar() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"partial").expect("write");
        let outcome = txn.abort();
        assert_eq!(outcome, CleanupOutcome::Clean);
        assert!(export_names(&fixture).is_empty());
        assert!(!fixture.temp.path().join("out/final.txt").exists());
    }

    #[test]
    fn abort_removes_sidecar_when_temp_already_missing() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"partial").expect("write");
        std::fs::remove_file(fixture.cache.join(STAGING_DIR_NAME).join(&txn.temp_name))
            .expect("remove temp");
        let outcome = txn.abort();
        assert_eq!(outcome, CleanupOutcome::Clean);
        assert!(export_names(&fixture).is_empty());
    }

    #[test]
    fn abort_keeps_sidecar_when_temp_unlink_fails() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"plaintext").expect("write");
        fault_injection::set_temp_unlink_failure(true);
        let outcome = txn.abort();
        fault_injection::set_temp_unlink_failure(false);
        assert_eq!(outcome, CleanupOutcome::SidecarRetained);
        assert_eq!(
            export_names(&fixture).len(),
            2,
            "temp 与 sidecar 都必须保留"
        );
        assert!(
            fixture
                .cache
                .join(STAGING_DIR_NAME)
                .join(&txn.sidecar_name)
                .exists()
        );
    }

    #[test]
    fn recovery_cleans_temp_after_temp_unlink_failure() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"plaintext").expect("write");
        fault_injection::set_temp_unlink_failure(true);
        assert_eq!(txn.abort(), CleanupOutcome::SidecarRetained);
        fault_injection::set_temp_unlink_failure(false);
        drop(txn);

        let report = fixture.staging.recover_orphans();
        assert_eq!(report.removed, 2);
        assert_eq!(report.failed, 0);
        assert!(export_names(&fixture).is_empty());
    }

    #[test]
    fn temp_unlink_failure_reports_cleanup_pending_after_commit_error() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"plaintext").expect("write");
        txn.sync_temp().expect("sync");
        fault_injection::set_rename_exdev(true);
        fault_injection::set_temp_unlink_failure(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_rename_exdev(false);
        fault_injection::set_temp_unlink_failure(false);
        assert_eq!(result.commit, ExportCommit::NotCommitted);
        assert_eq!(result.error.expect("error").code, "EXPORT_CLEANUP_PENDING");
        assert_eq!(export_names(&fixture).len(), 2);
        drop(txn);

        let report = fixture.staging.recover_orphans();
        assert_eq!(report.removed, 2);
        assert!(export_names(&fixture).is_empty());
    }

    #[test]
    fn sidecar_unlink_failure_reports_cleanup_pending() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"plaintext").expect("write");
        txn.sync_temp().expect("sync");
        fault_injection::set_rename_exdev(true);
        fault_injection::set_sidecar_unlink_failure(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_rename_exdev(false);
        fault_injection::set_sidecar_unlink_failure(false);
        assert_eq!(result.commit, ExportCommit::NotCommitted);
        assert_eq!(result.error.expect("error").code, "EXPORT_CLEANUP_PENDING");
        let names = export_names(&fixture);
        assert_eq!(names.len(), 1, "temp 已删除，sidecar 保留");
        assert!(
            names[0].ends_with(SIDECAR_SUFFIX),
            "剩余条目必须是 sidecar，而不是 temp"
        );
        drop(txn);

        let report = fixture.staging.recover_orphans();
        assert_eq!(report.removed, 1);
        assert!(export_names(&fixture).is_empty());
    }

    #[test]
    fn temp_sync_failure_aborts_without_temp() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"partial").expect("write");
        fault_injection::set_temp_sync_failure(true);
        let result = txn.sync_temp();
        fault_injection::set_temp_sync_failure(false);
        assert!(result.is_err());
        txn.abort();
        assert!(export_names(&fixture).is_empty());
        assert!(!fixture.temp.path().join("out/final.txt").exists());
    }

    #[test]
    fn commit_is_atomic_and_never_overwrites() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"first").expect("write");
        txn.sync_temp().expect("sync");
        let result = txn.commit(&fixture.staging, &fixture.target);
        assert_eq!(result.commit, ExportCommit::Committed);
        assert!(result.durability_confirmed);
        assert_eq!(
            std::fs::read(fixture.out.join("final.txt")).expect("final"),
            b"first"
        );
        assert!(export_names(&fixture).is_empty());

        let second = ExportTarget::open(
            &fixture.temp.path().join("vault"),
            &fixture.out.join("final.txt"),
            &[],
        )
        .expect_err("existing target");
        assert_eq!(second.code, "FILE_EXISTS");
        assert_eq!(
            std::fs::read(fixture.out.join("final.txt")).expect("final"),
            b"first"
        );
    }

    #[test]
    fn commit_runs_after_cancel_requested() {
        // 进入提交临界区后不再有取消检查；这里用“已取消标志”证明提交仍然完成。
        let fixture = fixture();
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"committed").expect("write");
        txn.sync_temp().expect("sync");
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
        let result = txn.commit(&fixture.staging, &fixture.target);
        assert_eq!(result.commit, ExportCommit::Committed);
        assert!(fixture.out.join("final.txt").exists());
    }

    #[test]
    fn target_dir_sync_failure_keeps_final_and_sidecar() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"durable?").expect("write");
        txn.sync_temp().expect("sync");
        fault_injection::set_target_sync_failure(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_target_sync_failure(false);
        assert_eq!(result.commit, ExportCommit::Committed);
        assert!(!result.durability_confirmed);
        assert_eq!(
            std::fs::read(fixture.out.join("final.txt")).expect("final"),
            b"durable?"
        );
        // sidecar 保留，等待恢复扫描；最终文件绝不删除。
        assert_eq!(export_names(&fixture).len(), 1);
        drop(txn);
        assert!(fixture.out.join("final.txt").exists());
    }

    #[test]
    fn staging_dir_sync_failure_keeps_sidecar() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"durable?").expect("write");
        txn.sync_temp().expect("sync");
        fault_injection::set_staging_sync_failure(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_staging_sync_failure(false);
        assert_eq!(result.commit, ExportCommit::Committed);
        assert!(!result.durability_confirmed);
        assert!(fixture.out.join("final.txt").exists());
        assert_eq!(export_names(&fixture).len(), 1);
    }

    #[test]
    fn commit_exdev_fails_closed_and_preserves_existing_target() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"new plaintext").expect("write");
        txn.sync_temp().expect("sync");
        assert_eq!(export_names(&fixture).len(), 2);

        // 在目标打开后制造已有文件，验证 EXDEV 路径不会覆盖、复制降级或误删它。
        std::fs::write(fixture.out.join("final.txt"), b"pre-existing").expect("existing");
        fault_injection::set_rename_exdev(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_rename_exdev(false);

        assert_eq!(result.commit, ExportCommit::NotCommitted);
        assert_eq!(
            result.error.expect("EXDEV error").code,
            "EXPORT_STAGING_UNAVAILABLE"
        );
        assert_eq!(
            std::fs::read(fixture.out.join("final.txt")).expect("existing final"),
            b"pre-existing"
        );
        assert!(export_names(&fixture).is_empty(), "temp/sidecar cleaned");
        let mut target_names: Vec<String> = std::fs::read_dir(&fixture.out)
            .expect("target dir")
            .map(|entry| {
                entry
                    .expect("target entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        target_names.sort();
        assert_eq!(
            target_names,
            vec!["final.txt".to_string()],
            "no plaintext temp or copy fallback artifact in target"
        );
    }

    #[test]
    fn commit_unknown_keeps_scene() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"unknown").expect("write");
        txn.sync_temp().expect("sync");
        fault_injection::set_rename_unknown(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_rename_unknown(false);
        assert_eq!(result.commit, ExportCommit::Unknown);
        assert!(!result.durability_confirmed);
        // temp + sidecar 都保留，最终文件不存在。
        assert_eq!(export_names(&fixture).len(), 2);
        assert!(!fixture.out.join("final.txt").exists());
        drop(txn);
        assert_eq!(export_names(&fixture).len(), 2);
    }

    #[test]
    fn recovery_cleans_sidecar_after_durability_warning() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"kept").expect("write");
        txn.sync_temp().expect("sync");
        fault_injection::set_target_sync_failure(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_target_sync_failure(false);
        assert_eq!(result.commit, ExportCommit::Committed);
        drop(txn);

        crate::export_staging::recover_staging_path(&fixture.cache.join(STAGING_DIR_NAME));
        assert!(export_names(&fixture).is_empty());
        assert!(fixture.out.join("final.txt").exists());
    }

    #[test]
    fn enoent_without_temp_reports_unknown_and_keeps_sidecar() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"plaintext").expect("write");
        txn.sync_temp().expect("sync");
        std::fs::remove_file(fixture.cache.join(STAGING_DIR_NAME).join(&txn.temp_name))
            .expect("remove temp");

        fault_injection::set_rename_enoent(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_rename_enoent(false);

        assert_eq!(result.commit, ExportCommit::Unknown);
        assert!(!result.durability_confirmed);
        assert_eq!(result.error.expect("error").code, "EXPORT_COMMIT_UNKNOWN");
        let names = export_names(&fixture);
        assert_eq!(names.len(), 1, "temp 已不存在，sidecar 必须保留");
        assert!(names[0].ends_with(SIDECAR_SUFFIX));
        assert!(!fixture.out.join("final.txt").exists());

        drop(txn);
        assert_eq!(export_names(&fixture).len(), 1, "Drop 不得删除 sidecar");
    }

    #[test]
    fn enoent_never_treats_existing_final_as_committed() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"plaintext").expect("write");
        txn.sync_temp().expect("sync");
        std::fs::write(fixture.out.join("final.txt"), b"foreign").expect("foreign final");
        std::fs::remove_file(fixture.cache.join(STAGING_DIR_NAME).join(&txn.temp_name))
            .expect("remove temp");

        fault_injection::set_rename_enoent(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_rename_enoent(false);

        assert_eq!(result.commit, ExportCommit::Unknown);
        assert!(!result.durability_confirmed);
        assert_eq!(
            std::fs::read(fixture.out.join("final.txt")).expect("foreign final"),
            b"foreign",
            "外部同名文件不得被当成本次提交，也不得被删除"
        );
        let names = export_names(&fixture);
        assert_eq!(names.len(), 1, "sidecar 必须保留");
        assert!(names[0].ends_with(SIDECAR_SUFFIX));
    }

    #[test]
    fn enoent_with_non_regular_final_stays_unknown() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"plaintext").expect("write");
        txn.sync_temp().expect("sync");
        std::fs::create_dir(fixture.out.join("final.txt")).expect("final dir");
        std::fs::remove_file(fixture.cache.join(STAGING_DIR_NAME).join(&txn.temp_name))
            .expect("remove temp");

        fault_injection::set_rename_enoent(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_rename_enoent(false);

        assert_eq!(result.commit, ExportCommit::Unknown);
        assert!(!result.durability_confirmed);
        assert!(
            fixture.out.join("final.txt").is_dir(),
            "目标目录项不得被当作提交结果"
        );
        let names = export_names(&fixture);
        assert_eq!(names.len(), 1);
        assert!(names[0].ends_with(SIDECAR_SUFFIX));
    }

    #[test]
    fn enoent_temp_stat_failure_stays_unknown_and_keeps_scene() {
        let fixture = fixture();
        let mut txn = ExportTxn::begin(&fixture.staging).expect("begin");
        txn.temp_mut().write_all(b"plaintext").expect("write");
        txn.sync_temp().expect("sync");

        fault_injection::set_rename_enoent(true);
        fault_injection::set_temp_stat_failure(true);
        let result = txn.commit(&fixture.staging, &fixture.target);
        fault_injection::set_rename_enoent(false);
        fault_injection::set_temp_stat_failure(false);

        assert_eq!(result.commit, ExportCommit::Unknown);
        assert!(!result.durability_confirmed);
        assert_eq!(result.error.expect("error").code, "EXPORT_COMMIT_UNKNOWN");
        // 检查失败不得被当作“不存在”而清理现场。
        let names = export_names(&fixture);
        assert_eq!(names.len(), 2, "temp 与 sidecar 都必须保留");
        assert!(names.iter().any(|name| name.ends_with(TEMP_SUFFIX)));
        assert!(names.iter().any(|name| name.ends_with(SIDECAR_SUFFIX)));
        assert!(!fixture.out.join("final.txt").exists());

        drop(txn);
        assert_eq!(export_names(&fixture).len(), 2, "Drop 不得清理现场");
    }

    /// 进程级 helper：未设置环境变量时直接通过。
    #[cfg(unix)]
    #[test]
    fn export_helper_process() {
        let Ok(spec) = std::env::var("HAVEN_EXPORT_HELPER") else {
            return;
        };
        let mut parts = spec.split('\n');
        let mode = parts.next().expect("mode");
        let cache = std::path::PathBuf::from(parts.next().expect("cache"));
        let out = std::path::PathBuf::from(parts.next().expect("out"));

        let target = ExportTarget::open(
            std::path::Path::new("/nonexistent-haven-vault"),
            &out.join("final.bin"),
            &[],
        )
        .expect("target");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        let mut txn = ExportTxn::begin(&staging).expect("begin");
        let payload = vec![0x5au8; 4 * 1024 * 1024];
        txn.temp_mut().write_all(&payload).expect("write");

        match mode {
            "writing" => {
                eprintln!("READY");
            }
            "synced" => {
                txn.sync_temp().expect("sync");
                eprintln!("SYNCED");
            }
            "committed" => {
                txn.sync_temp().expect("sync");
                let result = txn.commit(&staging, &target);
                eprintln!("COMMITTED:{:?}", result.commit);
            }
            _ => panic!("unknown mode"),
        }
        // 保持事务与锁直到父进程终止本进程。
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }

    #[cfg(unix)]
    fn spawn_helper(
        mode: &str,
        cache: &std::path::Path,
        out: &std::path::Path,
    ) -> std::process::Child {
        use std::process::{Command, Stdio};
        let exe = std::env::current_exe().expect("test exe");
        Command::new(exe)
            .args([
                "--exact",
                "export_txn::tests::export_helper_process",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(
                "HAVEN_EXPORT_HELPER",
                format!(
                    "{mode}\n{}\n{}",
                    cache.to_string_lossy(),
                    out.to_string_lossy()
                ),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn helper")
    }

    #[cfg(unix)]
    fn wait_marker(child: &mut std::process::Child, marker: &str) {
        use std::io::{BufRead, BufReader};
        let stderr = child.stderr.take().expect("stderr");
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).expect("read") == 0 {
                panic!("helper exited before marker {marker}");
            }
            if line.contains(marker) {
                return;
            }
        }
    }

    #[cfg(unix)]
    fn kill_and_wait(child: &mut std::process::Child) {
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn sigkill_during_write_leaves_recoverable_scene() {
        let fixture = fixture();
        let mut child = spawn_helper("writing", &fixture.cache, &fixture.out);
        wait_marker(&mut child, "READY");
        assert_eq!(export_names(&fixture).len(), 2);
        assert!(!fixture.out.join("final.bin").exists());
        kill_and_wait(&mut child);

        // 崩溃后现场保留，恢复扫描清理，目标目录始终没有明文临时文件。
        assert_eq!(export_names(&fixture).len(), 2);
        assert!(
            std::fs::read_dir(&fixture.out)
                .expect("out")
                .next()
                .is_none()
        );
        let report =
            crate::export_staging::recover_staging_path(&fixture.cache.join(STAGING_DIR_NAME));
        assert_eq!(report.failed, 0);
        assert_eq!(report.removed, 2);
        assert!(export_names(&fixture).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn sigkill_after_sync_before_commit_cleans_on_recovery() {
        let fixture = fixture();
        let mut child = spawn_helper("synced", &fixture.cache, &fixture.out);
        wait_marker(&mut child, "SYNCED");
        assert!(!fixture.out.join("final.bin").exists());
        kill_and_wait(&mut child);

        crate::export_staging::recover_staging_path(&fixture.cache.join(STAGING_DIR_NAME));
        assert!(export_names(&fixture).is_empty());
        assert!(!fixture.out.join("final.bin").exists());
    }

    #[cfg(unix)]
    #[test]
    fn sigkill_after_commit_keeps_final() {
        let fixture = fixture();
        let mut child = spawn_helper("committed", &fixture.cache, &fixture.out);
        wait_marker(&mut child, "COMMITTED:Committed");
        kill_and_wait(&mut child);

        let bytes = std::fs::read(fixture.out.join("final.bin")).expect("final");
        assert_eq!(bytes.len(), 4 * 1024 * 1024);
        crate::export_staging::recover_staging_path(&fixture.cache.join(STAGING_DIR_NAME));
        assert!(export_names(&fixture).is_empty());
        assert_eq!(
            std::fs::read(fixture.out.join("final.bin"))
                .expect("final")
                .len(),
            4 * 1024 * 1024
        );
    }

    #[cfg(unix)]
    #[test]
    fn multi_instance_recovery_skips_active_export() {
        let fixture = fixture();
        let mut child = spawn_helper("writing", &fixture.cache, &fixture.out);
        wait_marker(&mut child, "READY");

        let report =
            crate::export_staging::recover_staging_path(&fixture.cache.join(STAGING_DIR_NAME));
        assert!(report.skipped >= 1);
        assert_eq!(export_names(&fixture).len(), 2);
        assert!(!fixture.out.join("final.bin").exists());

        kill_and_wait(&mut child);
        crate::export_staging::recover_staging_path(&fixture.cache.join(STAGING_DIR_NAME));
        assert!(export_names(&fixture).is_empty());
    }

    /// 同路径换卷：原卷被强制卸载后挂载另一映像，提交必须失败关闭。
    /// 需要 `HAVEN_RUN_SWAP_TEST=1`，由进程级脚本执行。
    #[cfg(unix)]
    #[test]
    fn same_path_volume_swap_fails_closed() {
        use std::process::Command;

        if std::env::var_os("HAVEN_RUN_SWAP_TEST").is_none() {
            eprintln!("SKIP: HAVEN_RUN_SWAP_TEST 未设置");
            return;
        }
        let temp = TempDir::new().expect("temp");
        let image_a = temp.path().join("a.dmg");
        let image_b = temp.path().join("b.dmg");
        let mount = std::path::Path::new("/Volumes/HavenSwap");
        let create = |image: &std::path::Path| {
            let status = Command::new("hdiutil")
                .args([
                    "create",
                    "-size",
                    "32m",
                    "-fs",
                    "APFS",
                    "-volname",
                    "HavenSwap",
                    "-type",
                    "UDIF",
                ])
                .arg(image)
                .arg("-quiet")
                .status()
                .expect("hdiutil create");
            assert!(status.success());
        };
        let attach = |image: &std::path::Path| {
            let status = Command::new("hdiutil")
                .args(["attach"])
                .arg(image)
                .args(["-nobrowse", "-quiet"])
                .status()
                .expect("hdiutil attach");
            assert!(status.success());
        };
        let force_detach = || {
            Command::new("hdiutil")
                .args(["detach", "-force"])
                .arg(mount)
                .status()
                .expect("hdiutil detach")
                .success()
        };

        create(&image_a);
        create(&image_b);
        attach(&image_a);
        let out = mount.join("swap-out");
        std::fs::create_dir(&out).expect("out");
        let target = ExportTarget::open(
            std::path::Path::new("/nonexistent-haven-vault"),
            &out.join("final.bin"),
            &[],
        )
        .expect("target");
        let staging = StagingDir::resolve(&temp.path().join("cache"), &target).expect("staging");
        let mut txn = ExportTxn::begin(&staging).expect("begin");
        txn.temp_mut().write_all(b"volume").expect("write");
        txn.sync_temp().expect("sync");

        if !force_detach() {
            let _ = force_detach();
        }
        if mount.exists() {
            eprintln!("SKIP: 原卷无法强制卸载，换卷用例未执行");
            return;
        }
        attach(&image_b);
        assert!(mount.exists(), "新卷必须挂载到同一路径");

        let result = txn.commit(&staging, &target);
        assert_ne!(result.commit, ExportCommit::Committed);
        assert!(
            !mount.join("swap-out/final.bin").exists(),
            "旧卷的导出绝不能写入同路径的新卷"
        );
        let _ = force_detach();
    }
}
