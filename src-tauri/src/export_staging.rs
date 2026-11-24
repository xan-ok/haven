//! 导出暂存目录、卷身份识别与崩溃恢复。
//!
//! 安全边界：
//! - 导出的明文临时文件只存在于按卷选择的私有暂存目录，绝不写入用户目标目录。
//! - 暂存目录与临时文件都通过目录文件描述符（dirfd）相对操作，避免路径重新解析
//!   造成 TOCTOU；目录本身经过类型、所有者、权限、设备与文件系统身份校验。
//! - 恢复扫描只处理严格命名的 sidecar/temp，不读取也不删除最终文件；无法取得
//!   sidecar 锁的文件一律视为活动导出并跳过。
//! - 网络卷、只读卷、noowners 卷以及无法可靠识别身份的卷一律失败关闭。

use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::vault::VaultError;

/// 启动卷上应用私有暂存目录名。
pub const STAGING_DIR_NAME: &str = "export-staging";
/// 外置卷根目录下的隐藏暂存目录前缀。
pub const EXTERNAL_STAGING_PREFIX: &str = ".haven-export-staging-";
/// 导出临时文件前缀。
pub const TEMP_PREFIX: &str = ".haven-export-";
/// 导出临时文件后缀。
pub const TEMP_SUFFIX: &str = ".part";
/// 活动锁 sidecar 后缀。
pub const SIDECAR_SUFFIX: &str = ".lock";

const MNT_LOCAL_FLAG: u32 = 0x0000_1000;
const MNT_RDONLY_FLAG: u32 = 0x0000_0001;
const MNT_IGNORE_OWNERSHIP_FLAG: u32 = 0x0002_0000;

const S_IFMT_VALUE: u32 = 0o170000;
const S_IFDIR_VALUE: u32 = 0o040000;
const S_IFREG_VALUE: u32 = 0o100000;
const S_IFLNK_VALUE: u32 = 0o120000;

/// 同一进程内的恢复/创建串行化锁。
///
/// macOS 的 `flock` 以打开文件描述为对象，同进程内再次打开同一文件加锁的行为
/// 不足以作为可靠的活动判据；因此同进程内用该互斥锁串行化恢复扫描与 sidecar 创建。
static RECOVERY_MUTEX: Mutex<()> = Mutex::new(());

struct ActiveExport {
    dev: u64,
    ino: u64,
    token: String,
}

static ACTIVE_EXPORTS: Mutex<Vec<ActiveExport>> = Mutex::new(Vec::new());

/// 进程内活动导出注册表。`token` 只含随机十六进制字符。
pub(crate) fn register_active(dev: u64, ino: u64, token: &str) {
    let mut list = ACTIVE_EXPORTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    list.push(ActiveExport {
        dev,
        ino,
        token: token.to_string(),
    });
}

pub(crate) fn unregister_active(dev: u64, ino: u64, token: &str) {
    let mut list = ACTIVE_EXPORTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    list.retain(|item| !(item.dev == dev && item.ino == ino && item.token == token));
}

fn is_active(dev: u64, ino: u64, token: &str) -> bool {
    let list = ACTIVE_EXPORTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    list.iter()
        .any(|item| item.dev == dev && item.ino == ino && item.token == token)
}

/// 恢复扫描与 sidecar 创建共用的进程内串行化守卫。
pub(crate) fn recovery_guard() -> MutexGuard<'static, ()> {
    RECOVERY_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 卷身份：用于判断“暂存目录与目标目录是否同一文件系统实例”。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeIdentity {
    pub dev: u64,
    pub fsid: (i32, i32),
    pub mount_point: PathBuf,
    pub mount_from: OsString,
    pub fs_type: String,
    pub flags: u32,
}

impl VolumeIdentity {
    fn is_local(&self) -> bool {
        self.flags & MNT_LOCAL_FLAG != 0
    }

    fn is_read_only(&self) -> bool {
        self.flags & MNT_RDONLY_FLAG != 0
    }

    fn ignores_ownership(&self) -> bool {
        self.flags & MNT_IGNORE_OWNERSHIP_FLAG != 0
    }

    fn is_exportable(&self) -> bool {
        self.is_local() && !self.is_read_only() && !self.ignores_ownership()
    }
}

/// 两个身份是否属于同一文件系统实例。任何一方不可靠时由调用方失败关闭。
pub fn same_volume(left: &VolumeIdentity, right: &VolumeIdentity) -> bool {
    left.dev == right.dev
        && left.fsid == right.fsid
        && left.mount_from == right.mount_from
        && !left.mount_from.is_empty()
}

fn c_array_to_os_string<const N: usize>(value: &[libc::c_char; N]) -> OsString {
    let bytes: Vec<u8> = value
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    OsString::from_vec(bytes)
}

fn c_array_to_string<const N: usize>(value: &[libc::c_char; N]) -> String {
    c_array_to_os_string(value).to_string_lossy().into_owned()
}

pub(crate) fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut info) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

/// 捕获目录 fd 的卷身份。`fstatfs` 失败或关键字段缺失时失败关闭。
pub fn capture_volume(fd: RawFd) -> Result<VolumeIdentity, VaultError> {
    let info = fstat(fd).map_err(|_| VaultError::export_staging_unavailable())?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(fd, &mut stat) } != 0 {
        return Err(VaultError::export_staging_unavailable());
    }
    let mount_point = c_array_to_os_string(&stat.f_mntonname);
    let mount_from = c_array_to_os_string(&stat.f_mntfromname);
    let fs_type = c_array_to_string(&stat.f_fstypename);
    // libc 将 fsid_t 的内部字段设为私有；按 ABI 读取两个 i32。
    let fsid: [i32; 2] = unsafe { std::mem::transmute_copy(&stat.f_fsid) };
    if mount_point.as_os_str().is_empty() || mount_from.as_os_str().is_empty() || fs_type.is_empty()
    {
        return Err(VaultError::export_staging_unavailable());
    }
    Ok(VolumeIdentity {
        dev: info.st_dev as u64,
        fsid: (fsid[0], fsid[1]),
        mount_point: PathBuf::from(mount_point),
        mount_from,
        fs_type,
        flags: stat.f_flags,
    })
}

fn cstring(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "invalid name"))
}

fn open_dir(path: &Path) -> io::Result<File> {
    let c_path = cstring(path.as_os_str())?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: 路径字符串在调用期间有效；open 返回新的描述符所有权。
    let raw = unsafe { libc::open(c_path.as_ptr(), flags) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}

fn openat_dir(dir_fd: RawFd, name: &OsStr) -> io::Result<File> {
    let c_name = cstring(name)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let raw = unsafe { libc::openat(dir_fd, c_name.as_ptr(), flags) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}

fn mkdirat(dir_fd: RawFd, name: &OsStr, mode: libc::mode_t) -> io::Result<()> {
    let c_name = cstring(name)?;
    let result = unsafe { libc::mkdirat(dir_fd, c_name.as_ptr(), mode) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn unlinkat(dir_fd: RawFd, name: &OsStr) -> io::Result<()> {
    let c_name = cstring(name)?;
    let result = unsafe { libc::unlinkat(dir_fd, c_name.as_ptr(), 0) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn unlinkat_dir(dir_fd: RawFd, name: &OsStr) -> io::Result<()> {
    let c_name = cstring(name)?;
    let result = unsafe { libc::unlinkat(dir_fd, c_name.as_ptr(), libc::AT_REMOVEDIR) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn fsync_dir(file: &File) -> io::Result<()> {
    file.sync_all()
}

pub(crate) fn fsync_fd(fd: RawFd) -> io::Result<()> {
    // SAFETY: fd 在调用期间有效；fsync 不转移所有权。
    if unsafe { libc::fsync(fd) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct EntryInfo {
    dev: u64,
    ino: u64,
    mode: u32,
}

impl EntryInfo {
    fn is_dir(self) -> bool {
        self.mode & S_IFMT_VALUE == S_IFDIR_VALUE
    }

    fn is_file(self) -> bool {
        self.mode & S_IFMT_VALUE == S_IFREG_VALUE
    }

    fn is_symlink(self) -> bool {
        self.mode & S_IFMT_VALUE == S_IFLNK_VALUE
    }
}

fn entry_info(dir_fd: RawFd, name: &OsStr) -> io::Result<Option<EntryInfo>> {
    let c_name = cstring(name)?;
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::fstatat(
            dir_fd,
            c_name.as_ptr(),
            &mut info,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        let err = io::Error::last_os_error();
        if err.kind() == ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(err);
    }
    Ok(Some(EntryInfo {
        dev: info.st_dev as u64,
        ino: info.st_ino as u64,
        mode: info.st_mode as u32,
    }))
}

fn euid() -> u32 {
    // SAFETY: geteuid 无参数、无副作用。
    unsafe { libc::geteuid() as u32 }
}

fn canonical_dir(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path).ok().filter(|dir| dir.is_dir())
}

/// 已校验的导出目标目录。
#[derive(Debug)]
pub struct ExportTarget {
    dir_fd: File,
    final_name: OsString,
    identity: VolumeIdentity,
}

impl ExportTarget {
    pub fn dir_fd(&self) -> &File {
        &self.dir_fd
    }

    pub fn final_name(&self) -> &OsStr {
        &self.final_name
    }

    pub fn identity(&self) -> &VolumeIdentity {
        &self.identity
    }

    /// 校验并打开目标父目录，返回持有 dirfd 的目标。
    pub fn open(
        vault_dir: &Path,
        target: &Path,
        forbidden_dirs: &[PathBuf],
    ) -> Result<Self, VaultError> {
        if !target.is_absolute() {
            return Err(VaultError::export_target_invalid());
        }
        let final_name = target
            .file_name()
            .filter(|name| {
                !name.is_empty() && *name != OsStr::new(".") && *name != OsStr::new("..")
            })
            .ok_or_else(VaultError::export_target_invalid)?
            .to_os_string();
        let parent = target
            .parent()
            .ok_or_else(VaultError::export_target_invalid)?;

        let parent = fs::canonicalize(parent).map_err(|_| VaultError::export_target_invalid())?;
        let parent_info =
            fs::symlink_metadata(&parent).map_err(|_| VaultError::export_target_invalid())?;
        if parent_info.file_type().is_symlink() || !parent_info.is_dir() {
            return Err(VaultError::export_target_invalid());
        }
        let dir_fd = open_dir(&parent).map_err(|_| VaultError::export_target_invalid())?;
        let opened = fstat(dir_fd.as_raw_fd()).map_err(|_| VaultError::export_target_invalid())?;
        // 消除 canonicalize 与 open 之间目录被替换的窗口。
        if opened.st_dev as u64 != parent_info.dev() || opened.st_ino as u64 != parent_info.ino() {
            return Err(VaultError::export_target_invalid());
        }

        let final_path = parent.join(&final_name);
        let mut protected = Vec::new();
        if let Some(dir) = canonical_dir(vault_dir) {
            protected.push(dir);
        }
        for dir in forbidden_dirs {
            if let Some(dir) = canonical_dir(dir) {
                protected.push(dir);
            }
        }
        if protected.iter().any(|dir| final_path.starts_with(dir)) {
            return Err(VaultError::export_protected_path());
        }

        match entry_info(dir_fd.as_raw_fd(), &final_name)
            .map_err(|_| VaultError::export_target_invalid())?
        {
            Some(info) if info.is_dir() => return Err(VaultError::export_target_invalid()),
            // 已存在的目录项（含符号链接）一律拒绝覆盖。
            Some(_) => return Err(VaultError::file_exists()),
            None => {}
        }

        let identity = capture_volume(dir_fd.as_raw_fd())?;
        Ok(Self {
            dir_fd,
            final_name,
            identity,
        })
    }
}

/// 已校验的暂存目录。
#[derive(Debug)]
pub struct StagingDir {
    dir_fd: File,
    identity: VolumeIdentity,
    dev: u64,
    ino: u64,
}

impl StagingDir {
    pub fn dir_fd(&self) -> &File {
        &self.dir_fd
    }

    pub fn identity(&self) -> &VolumeIdentity {
        &self.identity
    }

    pub fn dev(&self) -> u64 {
        self.dev
    }

    pub fn ino(&self) -> u64 {
        self.ino
    }

    /// 依据目标所在卷选择暂存目录：启动卷用 app cache，本地外置卷用卷根隐藏目录。
    pub fn resolve(app_cache_dir: &Path, target: &ExportTarget) -> Result<Self, VaultError> {
        if !app_cache_dir.is_absolute() {
            return Err(VaultError::export_staging_unavailable());
        }
        if fs::symlink_metadata(app_cache_dir).is_err() {
            fs::create_dir_all(app_cache_dir)
                .map_err(|_| VaultError::export_staging_unavailable())?;
        }
        let cache_fd =
            open_dir(app_cache_dir).map_err(|_| VaultError::export_staging_unavailable())?;
        let cache_identity = capture_volume(cache_fd.as_raw_fd())?;

        let (parent_fd, dir_name) = if same_volume(&target.identity, &cache_identity) {
            if !cache_identity.is_exportable() {
                return Err(VaultError::export_staging_unavailable());
            }
            (cache_fd, OsString::from(STAGING_DIR_NAME))
        } else {
            if !target.identity.is_exportable() {
                return Err(VaultError::export_staging_unavailable());
            }
            let mount_fd = open_dir(&target.identity.mount_point)
                .map_err(|_| VaultError::export_staging_unavailable())?;
            let mount_identity = capture_volume(mount_fd.as_raw_fd())?;
            if !same_volume(&mount_identity, &target.identity) {
                return Err(VaultError::export_staging_unavailable());
            }
            (
                mount_fd,
                OsString::from(format!("{EXTERNAL_STAGING_PREFIX}{}", euid())),
            )
        };

        match mkdirat(parent_fd.as_raw_fd(), &dir_name, 0o700) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(_) => return Err(VaultError::export_staging_unavailable()),
        }
        let dir_fd = openat_dir(parent_fd.as_raw_fd(), &dir_name)
            .map_err(|_| VaultError::export_staging_unavailable())?;

        let info =
            fstat(dir_fd.as_raw_fd()).map_err(|_| VaultError::export_staging_unavailable())?;
        if (info.st_mode as u32) & S_IFMT_VALUE != S_IFDIR_VALUE
            || info.st_uid != euid()
            || (info.st_mode as u32) & 0o077 != 0
        {
            return Err(VaultError::export_staging_unavailable());
        }
        let identity = capture_volume(dir_fd.as_raw_fd())?;
        if !same_volume(&identity, &target.identity) || !identity.is_exportable() {
            return Err(VaultError::export_staging_unavailable());
        }
        // 目标目录本身不能就是暂存目录。
        let target_info =
            fstat(target.dir_fd().as_raw_fd()).map_err(|_| VaultError::export_target_invalid())?;
        if target_info.st_dev as u64 == info.st_dev as u64
            && target_info.st_ino as u64 == info.st_ino as u64
        {
            return Err(VaultError::export_target_invalid());
        }

        // 尽力降低 Spotlight 索引概率；失败不影响导出。
        let marker = OsStr::new(".metadata_never_index");
        if entry_info(dir_fd.as_raw_fd(), marker)
            .ok()
            .flatten()
            .is_none()
        {
            let _ = createat_private(dir_fd.as_raw_fd(), marker, libc::O_WRONLY, 0o600);
        }

        Ok(Self {
            dir_fd,
            identity,
            dev: info.st_dev as u64,
            ino: info.st_ino as u64,
        })
    }

    /// 导出前对本次使用的暂存目录做一次轻量扫描。
    /// 不传入父目录，避免把刚刚创建的空暂存目录删除。
    ///
    /// 与 `begin`/`commit` 共用同一把恢复锁：导出事务在创建 temp/sidecar 后、
    /// 注册 active 前存在一个窗口，必须由该锁串行化，否则并发导出的扫描可能
    /// 把尚未注册的活动事务误判为孤儿。
    pub fn recover_orphans(&self) -> RecoveryReport {
        let _guard = recovery_guard();
        recover_dir(self.dir_fd.as_raw_fd(), None, None)
    }
}

pub(crate) fn createat_private(
    dir_fd: RawFd,
    name: &OsStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<File> {
    let c_name = cstring(name)?;
    let flags = flags | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: 名称有效；O_EXCL 保证不会跟随/复用已存在的目录项。
    let raw = unsafe { libc::openat(dir_fd, c_name.as_ptr(), flags, mode as libc::c_int) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}

pub(crate) fn openat_read(dir_fd: RawFd, name: &OsStr) -> io::Result<File> {
    let c_name = cstring(name)?;
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let raw = unsafe { libc::openat(dir_fd, c_name.as_ptr(), flags) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}

pub(crate) fn openat_lock(dir_fd: RawFd, name: &OsStr) -> io::Result<File> {
    let c_name = cstring(name)?;
    let flags = libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let raw = unsafe { libc::openat(dir_fd, c_name.as_ptr(), flags) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}

/// 区分“存在 / 明确不存在 / 检查失败”，供提交状态判断使用。
///
/// 检查失败不能被当成“不存在”：调用方必须把 `Err` 视为状态无法确认。
pub(crate) fn entry_presence(dir_fd: RawFd, name: &OsStr) -> io::Result<bool> {
    entry_info(dir_fd, name).map(|info| info.is_some())
}

/// 恢复扫描结果，仅用于测试与诊断，不包含路径或名称。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryReport {
    pub removed: usize,
    pub skipped: usize,
    pub failed: usize,
}

fn strict_token(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(TEMP_PREFIX)?;
    let token = rest.strip_suffix(SIDECAR_SUFFIX)?;
    if token.len() == 16
        && token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Some(token)
    } else {
        None
    }
}

fn temp_name_for(token: &str) -> OsString {
    OsString::from(format!("{TEMP_PREFIX}{token}{TEMP_SUFFIX}"))
}

fn sidecar_name_for(token: &str) -> OsString {
    OsString::from(format!("{TEMP_PREFIX}{token}{SIDECAR_SUFFIX}"))
}

fn directory_names(dir_fd: RawFd) -> io::Result<Vec<OsString>> {
    // 通过 `.` 重新打开目录，得到独立的打开文件描述与偏移量；
    // `dup` 会共享目录偏移，重复扫描可能漏读条目。
    let fresh = openat_dir(dir_fd, OsStr::new("."))?;
    // SAFETY: fresh 的所有权转移给 fdopendir；closedir 会关闭它。
    let handle = unsafe { libc::fdopendir(std::os::fd::IntoRawFd::into_raw_fd(fresh)) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: handle 由 fdopendir 返回，循环期间有效；readdir 返回内部缓冲区。
        let entry = unsafe { libc::readdir(handle) };
        if entry.is_null() {
            break;
        }
        let name_bytes = unsafe { (*entry).d_name };
        let name = c_array_to_os_string(&name_bytes);
        if name == OsStr::new(".") || name == OsStr::new("..") {
            continue;
        }
        names.push(name);
    }
    unsafe {
        libc::closedir(handle);
    }
    Ok(names)
}

/// 恢复单个暂存目录。
///
/// `parent_fd`/`dir_name` 仅用于在目录完全为空时尝试 `AT_REMOVEDIR`；任一失败都不会
/// 删除整个目录。活动 sidecar 一律跳过，绝不因为扫描失败删除目录内容。
fn recover_dir(
    dir_fd: RawFd,
    parent_fd: Option<RawFd>,
    dir_name: Option<&OsStr>,
) -> RecoveryReport {
    let mut report = RecoveryReport::default();
    let staging_info = match fstat(dir_fd) {
        Ok(info) => info,
        Err(_) => {
            report.failed += 1;
            return report;
        }
    };
    let dev = staging_info.st_dev as u64;
    let ino = staging_info.st_ino as u64;
    let names = match directory_names(dir_fd) {
        Ok(names) => names,
        Err(_) => {
            report.failed += 1;
            return report;
        }
    };

    for name in names {
        let Some(text) = name.to_str() else {
            continue;
        };
        let Some(token) = strict_token(text) else {
            continue;
        };
        if is_active(dev, ino, token) {
            report.skipped += 1;
            continue;
        }
        let sidecar_name = sidecar_name_for(token);
        let temp_name = temp_name_for(token);

        let sidecar = match openat_lock(dir_fd, &sidecar_name) {
            Ok(file) => file,
            Err(_) => {
                // 名称可能已被并发清理；不视为错误。
                continue;
            }
        };
        let info = match fstat(sidecar.as_raw_fd()) {
            Ok(info) => info,
            Err(_) => {
                report.failed += 1;
                continue;
            }
        };
        if (info.st_mode as u32) & S_IFMT_VALUE != S_IFREG_VALUE
            || info.st_uid != euid()
            || (info.st_mode as u32) & 0o777 != 0o600
        {
            report.failed += 1;
            continue;
        }
        // 非阻塞排他锁：失败表示另一个进程正在导出。
        let locked = unsafe { libc::flock(sidecar.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked != 0 {
            report.skipped += 1;
            continue;
        }

        let mut removable = true;
        match entry_info(dir_fd, &temp_name) {
            Ok(None) => {}
            Ok(Some(entry)) if entry.is_file() => {
                let opened = match openat_read(dir_fd, &temp_name) {
                    Ok(file) => Some(file),
                    Err(_) => {
                        report.failed += 1;
                        removable = false;
                        None
                    }
                };
                if let Some(opened) = opened {
                    let opened_info = fstat(opened.as_raw_fd()).ok();
                    match opened_info {
                        Some(info)
                            if info.st_dev as u64 == entry.dev && info.st_ino == entry.ino =>
                        {
                            if unlinkat(dir_fd, &temp_name).is_ok() {
                                report.removed += 1;
                            } else {
                                report.failed += 1;
                                removable = false;
                            }
                        }
                        _ => {
                            // 名称在检查与打开之间被替换：跳过，保留现场。
                            report.failed += 1;
                            removable = false;
                        }
                    }
                }
            }
            Ok(Some(entry)) if entry.is_symlink() => {
                // 只删除链接本身，不跟随目标。
                if unlinkat(dir_fd, &temp_name).is_ok() {
                    report.removed += 1;
                } else {
                    report.failed += 1;
                    removable = false;
                }
            }
            Ok(Some(_)) => {
                // 目录或特殊文件：保留现场。
                report.failed += 1;
                removable = false;
            }
            Err(_) => {
                report.failed += 1;
                removable = false;
            }
        }

        if removable {
            match unlinkat(dir_fd, &sidecar_name) {
                Ok(()) => {
                    report.removed += 1;
                    let _ = fsync_fd(dir_fd);
                }
                Err(_) => {
                    report.failed += 1;
                }
            }
        }
    }

    let _ = fsync_fd(dir_fd);
    if report.failed == 0
        && let (Some(parent_fd), Some(name)) = (parent_fd, dir_name)
    {
        // 只有确认目录为空时才移除目录项。
        if directory_names(dir_fd)
            .map(|names| names.is_empty())
            .unwrap_or(false)
        {
            let _ = unlinkat_dir(parent_fd, name);
        }
    }
    report
}

/// 打开并校验一个暂存目录路径，然后执行恢复。仅测试直接调用。
#[cfg(test)]
pub fn recover_staging_path(path: &Path) -> RecoveryReport {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return RecoveryReport::default();
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return RecoveryReport {
            failed: 1,
            ..RecoveryReport::default()
        };
    }
    let Ok(file) = open_dir(path) else {
        return RecoveryReport {
            failed: 1,
            ..RecoveryReport::default()
        };
    };
    let Ok(info) = fstat(file.as_raw_fd()) else {
        return RecoveryReport {
            failed: 1,
            ..RecoveryReport::default()
        };
    };
    if (info.st_mode as u32) & S_IFMT_VALUE != S_IFDIR_VALUE
        || info.st_uid != euid()
        || (info.st_mode as u32) & 0o077 != 0
    {
        return RecoveryReport {
            failed: 1,
            ..RecoveryReport::default()
        };
    }
    let _guard = recovery_guard();
    let parent = path.parent().and_then(canonical_dir);
    let name = path.file_name().map(OsStr::to_os_string);
    match (parent.and_then(|dir| open_dir(&dir).ok()), name) {
        (Some(parent_fd), Some(name)) => recover_dir(
            file.as_raw_fd(),
            Some(parent_fd.as_raw_fd()),
            Some(name.as_os_str()),
        ),
        _ => recover_dir(file.as_raw_fd(), None, None),
    }
}

/// 启动/解锁时的全量恢复：应用暂存目录 + 当前挂载的本地可写外置卷。
pub fn recover_all(app_cache_dir: &Path) {
    let _guard = recovery_guard();
    recover_staging_locked(&app_cache_dir.join(STAGING_DIR_NAME));
    for mount in local_mounts() {
        let name = format!("{EXTERNAL_STAGING_PREFIX}{}", euid());
        let staging = mount.join(&name);
        if fs::symlink_metadata(&staging).is_ok() {
            recover_staging_locked(&staging);
        }
    }
}

fn recover_staging_locked(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return;
    }
    let Ok(file) = open_dir(path) else {
        return;
    };
    let Ok(info) = fstat(file.as_raw_fd()) else {
        return;
    };
    if (info.st_mode as u32) & S_IFMT_VALUE != S_IFDIR_VALUE
        || info.st_uid != euid()
        || (info.st_mode as u32) & 0o077 != 0
    {
        return;
    }
    let parent = path.parent().and_then(canonical_dir);
    let name = path.file_name().map(OsStr::to_os_string);
    match (parent.and_then(|dir| open_dir(&dir).ok()), name) {
        (Some(parent_fd), Some(name)) => {
            recover_dir(
                file.as_raw_fd(),
                Some(parent_fd.as_raw_fd()),
                Some(name.as_os_str()),
            );
        }
        _ => {
            recover_dir(file.as_raw_fd(), None, None);
        }
    }
}

fn local_mounts() -> Vec<PathBuf> {
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    let mut buffer: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; count as usize];
    let size = (count as usize * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    let written = unsafe { libc::getfsstat(buffer.as_mut_ptr(), size, libc::MNT_NOWAIT) };
    if written <= 0 {
        return Vec::new();
    }
    buffer.truncate(written as usize);
    buffer
        .into_iter()
        .filter(|stat| {
            stat.f_flags & MNT_LOCAL_FLAG != 0
                && stat.f_flags & MNT_RDONLY_FLAG == 0
                && stat.f_flags & MNT_IGNORE_OWNERSHIP_FLAG == 0
        })
        .map(|stat| PathBuf::from(c_array_to_os_string(&stat.f_mntonname)))
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn test_dir(temp: &TempDir, name: &str) -> PathBuf {
        let dir = temp.path().join(name);
        fs::create_dir(&dir).expect("create dir");
        dir
    }

    fn export_target(temp: &TempDir, name: &str) -> ExportTarget {
        let dir = test_dir(temp, "target");
        let vault = temp.path().join("vault");
        ExportTarget::open(&vault, &dir.join(name), &[]).expect("target")
    }

    #[test]
    fn staging_resolves_to_app_cache_on_boot_volume() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target = export_target(&temp, "out.txt");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        assert!(cache.join(STAGING_DIR_NAME).is_dir());
        let mode = fs::metadata(cache.join(STAGING_DIR_NAME))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(staging.identity().fsid, target.identity().fsid);
    }

    #[test]
    fn staging_rejects_symlink_directory() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let outside = test_dir(&temp, "outside");
        std::os::unix::fs::symlink(&outside, cache.join(STAGING_DIR_NAME)).expect("symlink");
        let target = export_target(&temp, "out.txt");
        let err = StagingDir::resolve(&cache, &target).expect_err("must reject");
        assert_eq!(err.code, "EXPORT_STAGING_UNAVAILABLE");
    }

    #[test]
    fn staging_rejects_wrong_mode() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let staging = cache.join(STAGING_DIR_NAME);
        fs::create_dir(&staging).expect("staging");
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).expect("chmod");
        let target = export_target(&temp, "out.txt");
        let err = StagingDir::resolve(&cache, &target).expect_err("must reject");
        assert_eq!(err.code, "EXPORT_STAGING_UNAVAILABLE");
    }

    #[test]
    fn target_inside_staging_is_rejected() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let staging = cache.join(STAGING_DIR_NAME);
        fs::create_dir(&staging).expect("staging");
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).expect("chmod");
        let target = ExportTarget::open(&temp.path().join("vault"), &staging.join("leak.txt"), &[])
            .expect("target");
        let err = StagingDir::resolve(&cache, &target).expect_err("must reject");
        assert_eq!(err.code, "EXPORT_TARGET_INVALID");
    }

    fn orphan(dir_fd: RawFd, token: &str, with_temp: bool) -> File {
        let sidecar_name = sidecar_name_for(token);
        let file = createat_private(dir_fd, &sidecar_name, libc::O_RDWR, 0o600).expect("sidecar");
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB);
        }
        if with_temp {
            let mut temp = createat_private(dir_fd, &temp_name_for(token), libc::O_WRONLY, 0o600)
                .expect("temp");
            temp.write_all(b"plaintext").expect("write");
        }
        file
    }

    #[test]
    fn recovery_removes_orphan_temp_and_sidecar() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target = export_target(&temp, "out.txt");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        let sidecar = orphan(staging.dir_fd().as_raw_fd(), "0123456789abcdef", true);
        drop(sidecar);

        let report = recover_staging_path(&cache.join(STAGING_DIR_NAME));
        assert_eq!(report.failed, 0);
        assert_eq!(report.removed, 2);
        let leftover = fs::read_dir(cache.join(STAGING_DIR_NAME))
            .expect("read")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .count();
        assert_eq!(leftover, 0);
    }

    #[test]
    fn recover_orphans_waits_for_recovery_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target = export_target(&temp, "out.txt");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        let sidecar = orphan(staging.dir_fd().as_raw_fd(), "aaaabbbbccccdddd", true);
        drop(sidecar);

        let dir = cache.join(STAGING_DIR_NAME);
        let guard = recovery_guard();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            attempt_tx.send(()).expect("attempt");
            let report = staging.recover_orphans();
            done_tx.send(report).expect("done");
        });

        // 通道只用于确定顺序：扫描线程已发出“开始”信号，此时锁仍由主线程持有。
        attempt_rx.recv().expect("scan thread started");
        assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        drop(guard);
        let report = done_rx.recv().expect("recovery completed");
        assert_eq!(report.removed, 2);
        handle.join().expect("join");
        assert!(!dir.join(temp_name_for("aaaabbbbccccdddd")).exists());
        assert!(!dir.join(sidecar_name_for("aaaabbbbccccdddd")).exists());
    }

    #[test]
    fn recovery_removes_sidecar_without_temp() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target = export_target(&temp, "out.txt");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        let sidecar = orphan(staging.dir_fd().as_raw_fd(), "fedcba9876543210", false);
        drop(sidecar);

        let report = recover_staging_path(&cache.join(STAGING_DIR_NAME));
        assert_eq!(report.removed, 1);
        assert!(
            !cache
                .join(STAGING_DIR_NAME)
                .join(sidecar_name_for("fedcba9876543210"))
                .exists()
        );
    }

    #[test]
    fn recovery_leaves_temp_without_sidecar() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target = export_target(&temp, "out.txt");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        createat_private(
            staging.dir_fd().as_raw_fd(),
            &temp_name_for("0011223344556677"),
            libc::O_WRONLY,
            0o600,
        )
        .expect("temp");

        let report = recover_staging_path(&cache.join(STAGING_DIR_NAME));
        assert_eq!(report.removed, 0);
        assert!(
            cache
                .join(STAGING_DIR_NAME)
                .join(temp_name_for("0011223344556677"))
                .exists()
        );
    }

    #[test]
    fn recovery_skips_locked_sidecar() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target = export_target(&temp, "out.txt");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        // 模拟另一个进程持续持有锁（同进程注册表不包含该 token）。
        let sidecar = orphan(staging.dir_fd().as_raw_fd(), "aaaabbbbccccdddd", true);

        let report = recover_staging_path(&cache.join(STAGING_DIR_NAME));
        assert!(report.skipped >= 1);
        assert!(
            cache
                .join(STAGING_DIR_NAME)
                .join(temp_name_for("aaaabbbbccccdddd"))
                .exists()
        );
        drop(sidecar);
    }

    #[test]
    fn recovery_never_touches_final_file() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target = export_target(&temp, "final.txt");
        fs::write(temp.path().join("target/final.txt"), b"final").expect("final");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        let sidecar = orphan(staging.dir_fd().as_raw_fd(), "1234567890abcdef", false);
        drop(sidecar);

        recover_staging_path(&cache.join(STAGING_DIR_NAME));
        assert_eq!(
            fs::read(temp.path().join("target/final.txt")).expect("final"),
            b"final"
        );
    }

    #[test]
    fn recovery_keeps_nonempty_staging_dir() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target = export_target(&temp, "out.txt");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        createat_private(
            staging.dir_fd().as_raw_fd(),
            OsStr::new("unrelated.txt"),
            libc::O_WRONLY,
            0o600,
        )
        .expect("unrelated");
        recover_staging_path(&cache.join(STAGING_DIR_NAME));
        assert!(cache.join(STAGING_DIR_NAME).is_dir());
    }

    #[test]
    fn recovery_single_failure_does_not_abort_all() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target = export_target(&temp, "out.txt");
        let staging = StagingDir::resolve(&cache, &target).expect("staging");
        let dir_fd = staging.dir_fd().as_raw_fd();
        let broken = orphan(dir_fd, "0000000000000001", false);
        drop(broken);
        // 让 sidecar 权限不符合预期，扫描应跳过而不是崩溃。
        let bad_name = sidecar_name_for("0000000000000001");
        let bad_path = cache.join(STAGING_DIR_NAME).join(&bad_name);
        fs::set_permissions(&bad_path, fs::Permissions::from_mode(0o644)).expect("chmod");
        let good = orphan(dir_fd, "0000000000000002", true);
        drop(good);

        let report = recover_staging_path(&cache.join(STAGING_DIR_NAME));
        assert!(report.removed >= 2);
        assert!(
            !cache
                .join(STAGING_DIR_NAME)
                .join(temp_name_for("0000000000000002"))
                .exists()
        );
    }

    #[test]
    fn error_messages_do_not_leak_paths_or_names() {
        let errors = [
            VaultError::export_staging_unavailable(),
            VaultError::export_commit_unknown(),
            VaultError::export_target_invalid(),
        ];
        for error in errors {
            assert!(!error.message.contains('/'));
            assert!(!error.message.contains(".haven"));
            assert!(!error.message.to_lowercase().contains("token"));
        }
    }

    /// 只读外置卷必须失败关闭；由进程级脚本通过环境变量提供只读挂载点。
    #[test]
    fn staging_rejects_readonly_external_mount() {
        let Some(mount) = std::env::var_os("HAVEN_TEST_READONLY_MOUNT") else {
            eprintln!("SKIP: HAVEN_TEST_READONLY_MOUNT 未设置，只读卷用例由 P12 脚本执行");
            return;
        };
        let mount = PathBuf::from(mount);
        let cache = TempDir::new().expect("boot temp");
        let target = ExportTarget::open(
            &cache.path().join("vault"),
            &mount.join("haven-readonly-target.txt"),
            &[],
        )
        .expect("target");
        let err = StagingDir::resolve(cache.path(), &target).expect_err("readonly must fail");
        assert_eq!(err.code, "EXPORT_STAGING_UNAVAILABLE");
    }

    #[test]
    fn volume_identity_mismatch_is_detected() {
        let temp = TempDir::new().expect("temp");
        let target = export_target(&temp, "out.txt");
        let mut identity = target.identity().clone();
        assert!(same_volume(&identity, target.identity()));
        identity.fsid = (identity.fsid.0.wrapping_add(1), identity.fsid.1);
        assert!(!same_volume(&identity, target.identity()));
        identity = target.identity().clone();
        identity.dev = identity.dev.wrapping_add(1);
        assert!(!same_volume(&identity, target.identity()));
        identity = target.identity().clone();
        identity.mount_from = OsString::from("/dev/other");
        assert!(!same_volume(&identity, target.identity()));
    }

    /// 外置卷测试需要真实挂载点；由进程级脚本通过环境变量提供一次性磁盘映像。
    #[test]
    fn staging_resolves_to_volume_root_on_external() {
        let Some(mount) = std::env::var_os("HAVEN_TEST_EXTERNAL_MOUNT") else {
            eprintln!("SKIP: HAVEN_TEST_EXTERNAL_MOUNT 未设置，外置卷用例由 P9 脚本执行");
            return;
        };
        let mount = PathBuf::from(mount);
        let out = mount.join("haven-external-out");
        if out.exists() {
            fs::remove_dir_all(&out).expect("clean out");
        }
        fs::create_dir(&out).expect("out");
        let cache = TempDir::new().expect("boot temp");
        let target = ExportTarget::open(&cache.path().join("vault"), &out.join("f.txt"), &[])
            .expect("target");

        let staging = StagingDir::resolve(cache.path(), &target).expect("staging");
        assert!(same_volume(staging.identity(), target.identity()));
        assert_eq!(
            staging.identity().mount_point,
            fs::canonicalize(&mount).expect("canonical mount")
        );
        assert!(
            mount
                .join(format!("{EXTERNAL_STAGING_PREFIX}{}", euid()))
                .is_dir()
        );
        let cache_identity = capture_volume(open_dir(cache.path()).expect("cache").as_raw_fd())
            .expect("cache identity");
        assert!(!same_volume(staging.identity(), &cache_identity));
    }

    #[test]
    fn nonlocal_network_identities_fail_before_staging_or_temp() {
        let temp = TempDir::new().expect("temp");
        let cache = test_dir(&temp, "cache");
        let target_dir = temp.path().join("target");
        let mut target = export_target(&temp, "network.txt");
        target.identity.dev = target.identity.dev.wrapping_add(1);
        target.identity.mount_point = temp.path().join("private-network-mount");
        target.identity.mount_from = OsString::from("//haven-test@example.invalid/private-share");
        target.identity.flags = 0;

        for fs_type in ["smbfs", "nfs"] {
            target.identity.fs_type = fs_type.to_string();
            assert!(!target.identity.is_exportable());
            let err = StagingDir::resolve(&cache, &target).expect_err("network must fail");
            assert_eq!(err.code, "EXPORT_STAGING_UNAVAILABLE");
            assert!(!cache.join(STAGING_DIR_NAME).exists());
            assert_eq!(
                fs::read_dir(&target_dir).expect("target dir").count(),
                0,
                "no plaintext temp or copy fallback artifact"
            );
            assert!(!err.message.contains('/'));
            assert!(!err.message.contains("private-network-mount"));
            assert!(!err.message.contains("example.invalid"));
            assert!(!err.message.contains("private-share"));
            assert!(!err.message.contains(temp.path().to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn readonly_and_network_flags_fail_closed() {
        let mut identity = VolumeIdentity {
            dev: 1,
            fsid: (1, 1),
            mount_point: PathBuf::from("/"),
            mount_from: OsString::from("/dev/disk1"),
            fs_type: "apfs".to_string(),
            flags: MNT_LOCAL_FLAG,
        };
        assert!(identity.is_exportable());
        identity.flags |= MNT_RDONLY_FLAG;
        assert!(!identity.is_exportable());
        identity.flags = MNT_LOCAL_FLAG | MNT_IGNORE_OWNERSHIP_FLAG;
        assert!(!identity.is_exportable());
        identity.flags = 0;
        assert!(!identity.is_exportable());
    }
}
