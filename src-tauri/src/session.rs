//! 应用会话状态：保险库位置、锁定/解锁状态与主密钥生命周期。
//!
//! 主密钥只保存在本模块的内存中，通过 `VaultStatus` 暴露给前端的只有状态与路径，
//! 不包含任何密钥、密码或派生结果。

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::backup;
use crate::export_staging;
use crate::file_store::{
    self, DeletedFile, ExportedFile, FileListItem, ImportSummary, ProgressEvent, VerifyReport,
};
use crate::restore;
use crate::vault::{self, VAULT_DIR_NAME, VaultError};

/// 应用配置目录中用于记住保险库位置的 JSON 文件。
pub const CONFIG_FILE: &str = "vault-location.json";

/// 返回给前端的保险库状态。永远不包含密钥。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub state: &'static str,
    pub vault_path: Option<String>,
    pub vault_name: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Default)]
pub struct Session {
    vault_dir: Option<PathBuf>,
    master_key: Option<Zeroizing<[u8; 32]>>,
    /// 解锁时记录的元数据身份指纹，用于识别同一路径被替换的保险库。
    /// 它只描述元数据内容，不代表物理磁盘身份。
    vault_fingerprint: Option<String>,
    unavailable: Option<String>,
    notice: Option<String>,
}

impl Session {
    pub fn uninitialized() -> Self {
        Self::default()
    }

    pub fn locked(vault_dir: PathBuf) -> Self {
        Self {
            vault_dir: Some(vault_dir),
            ..Self::default()
        }
    }

    pub fn unavailable(vault_dir: Option<PathBuf>, message: String) -> Self {
        Self {
            vault_dir,
            unavailable: Some(message),
            ..Self::default()
        }
    }

    pub fn mark_unlocked(
        &mut self,
        vault_dir: PathBuf,
        master_key: Zeroizing<[u8; 32]>,
        fingerprint: String,
    ) {
        self.vault_dir = Some(vault_dir);
        self.master_key = Some(master_key);
        self.vault_fingerprint = Some(fingerprint);
        self.unavailable = None;
        self.notice = None;
    }

    pub fn mark_locked(&mut self, vault_dir: PathBuf) {
        self.master_key = None;
        // 打开的可能是另一个保险库，旧指纹不能沿用，解锁成功后重新记录。
        self.vault_fingerprint = None;
        self.vault_dir = Some(vault_dir);
        self.unavailable = None;
        self.notice = None;
    }

    /// 当前解锁会话记录的保险库身份指纹；锁定后仍保留，直到下次解锁覆盖。
    pub fn vault_fingerprint(&self) -> Option<&str> {
        self.vault_fingerprint.as_deref()
    }

    /// 锁定并尽力清理内存中的主密钥。
    pub fn lock(&mut self) {
        // `take` 会把 `Zeroizing` 移出并立即触发清零。
        self.master_key = None;
        self.notice = None;
    }

    pub fn vault_dir(&self) -> Option<&Path> {
        self.vault_dir.as_deref()
    }

    /// 需要主密钥的业务（导入/校验）必须先经过这里；锁定状态下返回明确错误。
    pub fn require_master_key(&self) -> Result<&Zeroizing<[u8; 32]>, VaultError> {
        self.master_key
            .as_ref()
            .ok_or_else(VaultError::not_unlocked)
    }

    pub fn is_unlocked(&self) -> bool {
        self.master_key.is_some()
    }

    /// 生成状态快照，并消费一次性提示（例如位置保存失败）。
    pub fn take_status(&mut self) -> VaultStatus {
        let notice = self.notice.take();
        let vault_path = self
            .vault_dir
            .as_ref()
            .map(|dir| dir.to_string_lossy().into_owned());
        let vault_name = vault_path.as_ref().map(|_| VAULT_DIR_NAME.to_string());

        if let Some(message) = &self.unavailable {
            return VaultStatus {
                state: "unavailable",
                vault_path,
                vault_name,
                message: Some(message.clone()),
            };
        }

        if vault_path.is_some() {
            let state = if self.master_key.is_some() {
                "unlocked"
            } else {
                "locked"
            };
            return VaultStatus {
                state,
                vault_path,
                vault_name,
                message: notice,
            };
        }

        VaultStatus {
            state: "uninitialized",
            vault_path: None,
            vault_name: None,
            message: notice,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultLocation {
    vault_dir: String,
}

const MAX_CONFIG_BYTES: u64 = 64 * 1024;

fn read_location(path: &Path) -> Result<Option<PathBuf>, VaultError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(VaultError::disk()),
    };
    let mut bytes = Vec::new();
    std::io::Read::take(file, MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| VaultError::disk())?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(VaultError::new(
            "METADATA_INVALID",
            "保险库位置信息已损坏。",
        ));
    }
    let location: VaultLocation = serde_json::from_slice(&bytes)
        .map_err(|_| VaultError::new("METADATA_INVALID", "保险库位置信息已损坏。"))?;
    if location.vault_dir.is_empty() {
        return Err(VaultError::new(
            "METADATA_INVALID",
            "保险库位置信息已损坏。",
        ));
    }
    let vault_dir = PathBuf::from(location.vault_dir);
    if !vault_dir.is_absolute() {
        return Err(VaultError::new(
            "METADATA_INVALID",
            "保险库位置信息已损坏。",
        ));
    }
    Ok(Some(vault_dir))
}

fn write_location(path: &Path, vault_dir: &Path) -> Result<(), VaultError> {
    let dir_text = vault_dir
        .to_str()
        .ok_or_else(|| VaultError::new("PATH_INVALID", "保险库位置包含不支持的字符。"))?;
    let payload = serde_json::to_vec_pretty(&VaultLocation {
        vault_dir: dir_text.to_string(),
    })
    .map_err(|_| VaultError::internal())?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| VaultError::disk())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }

    let temp_path = path.with_file_name(format!(".{CONFIG_FILE}.tmp-{}", vault::random_suffix()?));

    let write_result = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        // create_new 拒绝已存在的目录项，避免跟随他人预置的符号链接或复用旧临时文件。
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path)?;
        std::io::Write::write_all(&mut file, &payload)?;
        file.sync_all()?;
        fs::rename(&temp_path, path)
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
        return Err(VaultError::disk());
    }
    Ok(())
}

/// 协调导入/校验任务与锁定流程。
///
/// 每个任务在开始时快照一个取消代际；取消操作只递增代际，因此一个任务的结束
/// 不可能清除另一个任务（或锁定流程）发出的取消请求。锁定等待 `active` 归零后
/// 才清理主密钥，超时则拒绝锁定，不会出现“已锁定但任务仍持有密钥”的状态。
#[derive(Default)]
struct GateState {
    active: usize,
    exclusive: bool,
}

struct ImportGate {
    state: Mutex<GateState>,
    idle: Condvar,
    generation: AtomicU64,
    serial: Mutex<()>,
}

impl ImportGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            idle: Condvar::new(),
            generation: AtomicU64::new(0),
            serial: Mutex::new(()),
        }
    }

    fn begin(self: &Arc<Self>) -> Result<ImportPermit, VaultError> {
        let generation = self.generation.load(Ordering::SeqCst);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.exclusive {
            return Err(VaultError::delete_busy());
        }
        state.active += 1;
        Ok(ImportPermit {
            gate: Arc::clone(self),
            generation,
            exclusive: false,
        })
    }

    /// 删除采用快速失败的独占许可：不与任何活动任务重叠，也不长时间等待锁定。
    fn begin_exclusive(self: &Arc<Self>) -> Result<ImportPermit, VaultError> {
        let generation = self.generation.load(Ordering::SeqCst);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.exclusive || state.active != 0 {
            return Err(VaultError::delete_busy());
        }
        state.exclusive = true;
        state.active = 1;
        Ok(ImportPermit {
            gate: Arc::clone(self),
            generation,
            exclusive: true,
        })
    }

    fn try_serial(&self) -> Result<MutexGuard<'_, ()>, VaultError> {
        self.serial
            .try_lock()
            .map_err(|_| VaultError::import_in_progress())
    }

    fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// 等待任务退出；返回 `true` 表示已全部退出，超时返回 `false`。
    fn wait_idle(&self, timeout: Duration) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (state, result) = self
            .idle
            .wait_timeout_while(state, timeout, |state| state.active > 0)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !result.timed_out() && state.active == 0
    }
}

struct ImportPermit {
    gate: Arc<ImportGate>,
    generation: u64,
    exclusive: bool,
}

impl ImportPermit {
    fn is_cancelled(&self) -> bool {
        self.gate.generation.load(Ordering::SeqCst) != self.generation
    }
}

impl Drop for ImportPermit {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active = state.active.saturating_sub(1);
        if self.exclusive {
            state.exclusive = false;
        }
        self.gate.idle.notify_all();
    }
}

struct Inner {
    session: Mutex<Session>,
    operation: Mutex<()>,
    gate: Arc<ImportGate>,
    config_path: PathBuf,
    app_cache_dir: PathBuf,
    exiting: AtomicBool,
    exit_phase: AtomicU8,
}

/// 退出流程阶段。只允许 `Running -> Stopping -> Done` 单向流转。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitPhase {
    Running,
    Stopping,
    Done,
}

const EXIT_RUNNING: u8 = 0;
const EXIT_STOPPING: u8 = 1;
const EXIT_DONE: u8 = 2;

/// 退出流程等待业务操作临界区的上限。
///
/// 退出可以放弃等待正在执行的命令，但必须在有界时间内继续清理主密钥。
const EXIT_OPERATION_BUDGET: Duration = Duration::from_millis(300);

/// 在有界时间内尝试取得 `operation` 锁。
///
/// 返回 `true` 表示已取得（poison 也视为可继续），`false` 表示预算耗尽。
/// 调用方在返回 `false` 时不得再次阻塞等待；会话提交闸门负责剩余的一致性。
fn acquire_operation_bounded(inner: &Inner, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        match inner.operation.try_lock() {
            Ok(_guard) => break true,
            Err(TryLockError::Poisoned(poisoned)) => {
                drop(poisoned.into_inner());
                break true;
            }
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

impl AppState {
    /// 启动时依据记忆的位置决定初始状态。绝不自动创建新保险库。
    pub fn bootstrap(config_path: PathBuf, app_cache_dir: PathBuf) -> Self {
        let session = match read_location(&config_path) {
            Ok(Some(dir)) => match vault::read_metadata(&dir)
                .and_then(|metadata| vault::validate_metadata(&metadata))
            {
                Ok(_) => Session::locked(dir),
                Err(err) => Session::unavailable(Some(dir), err.message),
            },
            Ok(None) => Session::uninitialized(),
            Err(err) => Session::unavailable(None, err.message),
        };

        Self {
            inner: Arc::new(Inner {
                session: Mutex::new(session),
                operation: Mutex::new(()),
                gate: Arc::new(ImportGate::new()),
                config_path,
                app_cache_dir,
                exiting: AtomicBool::new(false),
                exit_phase: AtomicU8::new(EXIT_RUNNING),
            }),
        }
    }

    fn session(&self) -> MutexGuard<'_, Session> {
        self.inner
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn operation(&self) -> MutexGuard<'_, ()> {
        self.inner
            .operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn exit_phase(&self) -> ExitPhase {
        match self.inner.exit_phase.load(Ordering::SeqCst) {
            EXIT_STOPPING => ExitPhase::Stopping,
            EXIT_DONE => ExitPhase::Done,
            _ => ExitPhase::Running,
        }
    }

    /// 幂等退出入口：只有第一次调用会启动后台清理线程。
    ///
    /// 主线程只负责调用本方法并立即返回，清理（取消任务、最多等待 2 秒、清零主密钥）
    /// 在独立线程完成，避免与通过主线程派发的进度事件相互等待造成死锁。
    pub fn begin_exit<F>(&self, timeout: Duration, done: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if self
            .inner
            .exit_phase
            .compare_exchange(
                EXIT_RUNNING,
                EXIT_STOPPING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return;
        }
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // 先拒绝新的任务登记并取消已登记任务，再尝试进入业务操作临界区；
                // 这样即使操作锁被长时间占用，新任务也会立刻被拒绝。
                inner.exiting.store(true, Ordering::SeqCst);
                inner.gate.cancel();
                // 退出只做有界等待：超时后继续清理，剩余一致性问题由
                // create/open/unlock 在会话提交处的退出闸门负责。
                let _operation_acquired = acquire_operation_bounded(&inner, EXIT_OPERATION_BUDGET);
                // 提交临界区内的导出不会响应取消，因此最多等待 2 秒。
                let _ = inner.gate.wait_idle(timeout);
                let mut session = inner
                    .session
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                session.lock();
            }));
            inner.exit_phase.store(EXIT_DONE, Ordering::SeqCst);
            done();
        });
    }

    /// 清扫应用暂存目录与当前挂载的本地外置卷暂存目录。
    ///
    /// 启动、解锁与导出前均会调用；失败只影响清理，不影响保险库可用性。
    pub fn recover_exports(&self) {
        export_staging::recover_all(&self.inner.app_cache_dir);
    }

    fn persist_location(&self, vault_dir: &Path) -> Result<(), VaultError> {
        write_location(&self.inner.config_path, vault_dir)
    }

    pub fn status(&self) -> VaultStatus {
        self.session().take_status()
    }

    /// 创建保险库。成功后会直接进入已解锁状态。
    pub fn create_vault(
        &self,
        parent: PathBuf,
        password: String,
    ) -> Result<VaultStatus, VaultError> {
        let _operation = self.operation();
        let password = Zeroizing::new(password);
        if !parent.is_absolute() {
            return Err(VaultError::new(
                "PATH_INVALID",
                "所选位置不可用，请重新选择目录。",
            ));
        }

        {
            let session = self.session();
            if session.is_unlocked() {
                return Err(VaultError::new(
                    "ALREADY_UNLOCKED",
                    "保险库已处于解锁状态，请先锁定。",
                ));
            }
        }

        let (vault_dir, master_key) = vault::create_vault_dir(&parent, &password)?;
        file_store::prepare_vault(&vault_dir, &master_key)?;
        let fingerprint = vault::identity_fingerprint(&vault::read_metadata(&vault_dir)?);

        let mut session = self.session();
        // 退出闸门：检查与写入都在同一个 session 守卫内完成，期间不释放锁。
        // 拒绝时已创建的保险库目录保留，但不得提交解锁状态或持久化位置。
        if self.inner.exiting.load(Ordering::SeqCst) {
            return Err(VaultError::app_exiting());
        }
        session.mark_unlocked(vault_dir.clone(), master_key, fingerprint);
        if self.persist_location(&vault_dir).is_err() {
            session.notice = Some(
                "保险库已创建，但无法记住位置。下次请通过「打开已有保险库」进入。".to_string(),
            );
        }
        Ok(session.take_status())
    }

    /// 打开已有保险库目录，只做校验并进入锁定状态，不在此步骤校验密码。
    pub fn open_vault(&self, vault_dir: PathBuf) -> Result<VaultStatus, VaultError> {
        let _operation = self.operation();
        if !vault_dir.is_absolute() {
            return Err(VaultError::new(
                "PATH_INVALID",
                "所选位置不可用，请重新选择目录。",
            ));
        }

        {
            let session = self.session();
            if session.is_unlocked() {
                return Err(VaultError::new(
                    "ALREADY_UNLOCKED",
                    "保险库已处于解锁状态，请先锁定。",
                ));
            }
        }

        let metadata = vault::read_metadata(&vault_dir)?;
        vault::validate_metadata(&metadata)?;

        let mut session = self.session();
        // 退出闸门：拒绝时不得写入位置配置。
        if self.inner.exiting.load(Ordering::SeqCst) {
            return Err(VaultError::app_exiting());
        }
        session.mark_locked(vault_dir.clone());
        if self.persist_location(&vault_dir).is_err() {
            session.notice =
                Some("已打开保险库，但无法记住位置，下次需要重新选择目录。".to_string());
        }
        Ok(session.take_status())
    }

    /// 使用主密码解锁。只有认证成功才会把主密钥写入会话。
    pub fn unlock_vault(&self, password: String) -> Result<VaultStatus, VaultError> {
        let _operation = self.operation();
        let password = Zeroizing::new(password);

        let vault_dir = {
            let session = self.session();
            if session.is_unlocked() {
                return Err(VaultError::new(
                    "ALREADY_UNLOCKED",
                    "保险库已处于解锁状态，请先锁定。",
                ));
            }
            session
                .vault_dir()
                .map(Path::to_path_buf)
                .ok_or_else(|| VaultError::new("VAULT_NOT_FOUND", "尚未打开任何保险库。"))?
        };

        let metadata = vault::read_metadata(&vault_dir)?;
        let fingerprint = vault::identity_fingerprint(&metadata);
        let master_key = vault::unlock_vault_dir(&vault_dir, &password)?;
        // 清理上次异常退出留下的临时数据；绝不删除未被索引确认的密文。
        file_store::prepare_vault(&vault_dir, &master_key)?;

        let mut session = self.session();
        // 退出闸门：拒绝时未提交的本地主密钥随 `Zeroizing` 离开作用域清零，
        // 且不会执行后续的索引恢复副作用。
        if self.inner.exiting.load(Ordering::SeqCst) {
            return Err(VaultError::app_exiting());
        }
        session.mark_unlocked(vault_dir, master_key, fingerprint);
        let status = session.take_status();
        drop(session);
        // 解锁成功后同步做一次尽力恢复；失败不影响解锁结果。
        self.recover_exports();
        Ok(status)
    }

    /// 手动锁定并清理主密钥。
    ///
    /// 先要求进行中的任务安全终止，再清零主密钥；锁定结束后不会再有任务持有可用密钥。
    pub fn lock(&self) -> Result<VaultStatus, VaultError> {
        self.lock_with_timeout(Duration::from_secs(10))
    }

    fn lock_with_timeout(&self, timeout: Duration) -> Result<VaultStatus, VaultError> {
        let _operation = self.operation();
        self.inner.gate.cancel();
        if !self.inner.gate.wait_idle(timeout) {
            // 仍有任务在执行：拒绝锁定并保留主密钥，避免出现“已锁定但任务仍在使用密钥”。
            // 取消代际保持递增，进行中的任务会在下一个检查点安全退出，届时可再次锁定。
            return Err(VaultError::lock_busy());
        }
        let mut session = self.session();
        session.lock();
        Ok(session.take_status())
    }

    /// 退出路径使用：不等待进行中的任务，直接清理会话中的主密钥。
    ///
    /// 与 `lock()` 不同，这里不发生任何阻塞或磁盘 I/O，避免在主线程与导入任务的
    /// 事件派发之间相互等待；进程即将退出，密钥清零是唯一目标。
    pub fn shutdown(&self) {
        let mut session = self.session();
        session.lock();
    }

    /// 请求取消正在进行的任务。
    ///
    /// 取消通过递增代际实现，空闲时调用也不会影响后续新建的任务。
    pub fn request_cancel(&self) {
        self.inner.gate.cancel();
    }

    /// 校验保险库目录、元数据与身份指纹仍然一致；调用方必须已持有 `operation` 锁。
    ///
    /// 任何 I/O 失败或指纹不一致都视为不可用，不做“读取失败即成功”的降级。
    fn ensure_vault_mounted(&self, vault_dir: &Path, expected: &str) -> Result<(), VaultError> {
        let metadata =
            vault::read_metadata(vault_dir).map_err(|_| VaultError::vault_unavailable())?;
        if vault::identity_fingerprint(&metadata) != expected {
            return Err(VaultError::vault_unavailable());
        }
        Ok(())
    }

    /// 在持有 `operation` 锁时复制主密钥，并确认保险库仍然可用。
    ///
    /// 校验失败会立即清空会话主密钥（`Session::lock`），但保留 `vault_dir`，
    /// 以便磁盘恢复后用户重新输入密码解锁。
    fn prepare_unlocked_task(&self) -> Result<(Zeroizing<[u8; 32]>, PathBuf), VaultError> {
        let session = self.session();
        let master_key = session.require_master_key()?;
        let vault_dir = session
            .vault_dir()
            .ok_or_else(VaultError::vault_unavailable)?
            .to_path_buf();
        let key_copy = Zeroizing::new(**master_key);
        let fingerprint = session.vault_fingerprint().map(str::to_string);
        drop(session);

        let unavailable = || {
            self.session().lock();
            VaultError::vault_unavailable()
        };

        let Some(fingerprint) = fingerprint else {
            return Err(unavailable());
        };
        if self.ensure_vault_mounted(&vault_dir, &fingerprint).is_err() {
            return Err(unavailable());
        }
        Ok((key_copy, vault_dir))
    }

    /// 在锁内登记任务并复制一份零化密钥，随后释放所有互斥锁做磁盘 I/O。
    fn begin_task(&self) -> Result<(ImportPermit, Zeroizing<[u8; 32]>, PathBuf), VaultError> {
        let _operation = self.operation();
        if self.inner.exiting.load(Ordering::SeqCst) {
            return Err(VaultError::app_exiting());
        }
        let (key_copy, vault_dir) = self.prepare_unlocked_task()?;
        let permit = self.inner.gate.begin()?;
        Ok((permit, key_copy, vault_dir))
    }

    /// FR-46：登记一个不依赖会话主密钥的任务许可。
    ///
    /// 备份恢复只读取外部备份目录并写入用户指定的新目录，不需要解锁状态；
    /// 锁定仍会通过取消代际安全终止该任务并等待其结束。
    fn begin_passive_task(&self) -> Result<ImportPermit, VaultError> {
        let _operation = self.operation();
        if self.inner.exiting.load(Ordering::SeqCst) {
            return Err(VaultError::app_exiting());
        }
        self.inner.gate.begin()
    }

    /// 当前已打开的保险库目录（锁定或解锁均可；从未打开时为空）。
    ///
    /// 仅用于恢复时防止目标目录落入当前保险库，不参与恢复的密码验证。
    fn current_vault_dir(&self) -> Option<PathBuf> {
        let session = self.session();
        session.vault_dir().map(Path::to_path_buf)
    }

    fn begin_exclusive_task(
        &self,
    ) -> Result<(ImportPermit, Zeroizing<[u8; 32]>, PathBuf), VaultError> {
        let _operation = self.operation();
        if self.inner.exiting.load(Ordering::SeqCst) {
            return Err(VaultError::app_exiting());
        }
        let (key_copy, vault_dir) = self.prepare_unlocked_task()?;
        let permit = self.inner.gate.begin_exclusive()?;
        Ok((permit, key_copy, vault_dir))
    }

    /// 提交状态不明时立即撤销当前会话的解密能力，后续只能重新解锁恢复。
    fn recovery_error(&self, error: VaultError) -> VaultError {
        if error.code == "VAULT_RECOVERY_REQUIRED" {
            let mut session = self.session();
            session.master_key = None;
            session.unavailable = None;
            session.notice = None;
        }
        error
    }

    /// 串行导入多个文件。单个失败不影响其余文件；锁定会安全终止导入。
    pub fn import_files(
        &self,
        paths: Vec<String>,
        progress: &dyn Fn(ProgressEvent),
    ) -> Result<ImportSummary, VaultError> {
        let (permit, key, vault_dir) = self.begin_task()?;
        let _serial = self.inner.gate.try_serial()?;
        let cancel = || permit.is_cancelled();
        file_store::import_batch(&vault_dir, &key, &paths, &cancel, progress)
            .map_err(|err| self.recovery_error(err))
    }

    /// 读取加密索引并返回文件列表。列表只在解锁状态下可用。
    ///
    /// 与导入一样登记任务许可：锁定成功前会等待本次读取结束，不会出现
    /// “已清除主密钥但仍有代码在用密钥副本读索引”的状态。
    pub fn list_files(&self) -> Result<Vec<FileListItem>, VaultError> {
        let (_permit, key, vault_dir) = self.begin_task()?;
        let _vault_lock = file_store::VaultLock::acquire_shared(&vault_dir)?;
        let mut entries = file_store::Index::load(&vault_dir, &key)?.entries;
        entries.sort_by(|left, right| right.imported_at.cmp(&left.imported_at));
        Ok(entries.into_iter().map(FileListItem::from).collect())
    }

    /// 在内存中解密索引中登记的一张图片；明文不落盘，密钥不出后端。
    pub fn preview_image(&self, id: String) -> Result<Vec<u8>, VaultError> {
        let (permit, key, vault_dir) = self.begin_task()?;
        let _vault_lock = file_store::VaultLock::acquire_shared(&vault_dir)?;
        let index = file_store::Index::load(&vault_dir, &key)?;
        let entry = index
            .entries
            .into_iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| VaultError::new("FILE_NOT_FOUND", "找不到该文件。"))?;
        let cancel = || permit.is_cancelled();
        match file_store::preview_image(&vault_dir, &entry, &key, &cancel) {
            // 导入与预览共用取消代际，给预览返回更准确的错误码。
            Err(err) if err.code == "IMPORT_CANCELLED" => {
                Err(VaultError::new("PREVIEW_CANCELLED", "预览已取消。"))
            }
            result => result,
        }
    }

    /// 在内存中解密视频；明文不落盘，密钥不出后端。
    pub fn preview_video(&self, id: String) -> Result<Vec<u8>, VaultError> {
        let (permit, key, vault_dir) = self.begin_task()?;
        let _vault_lock = file_store::VaultLock::acquire_shared(&vault_dir)?;
        let index = file_store::Index::load(&vault_dir, &key)?;
        let entry = index
            .entries
            .into_iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| VaultError::new("FILE_NOT_FOUND", "找不到该文件。"))?;
        let cancel = || permit.is_cancelled();
        match file_store::preview_video(&vault_dir, &entry, &key, &cancel) {
            Err(err) if err.code == "IMPORT_CANCELLED" => {
                Err(VaultError::new("PREVIEW_CANCELLED", "预览已取消。"))
            }
            result => result,
        }
    }

    /// 在内存中解密图片并生成 PNG 缩略图；不落盘，密钥不出后端。
    pub fn preview_thumbnail(&self, id: String) -> Result<Vec<u8>, VaultError> {
        let (permit, key, vault_dir) = self.begin_task()?;
        let _vault_lock = file_store::VaultLock::acquire_shared(&vault_dir)?;
        let index = file_store::Index::load(&vault_dir, &key)?;
        let entry = index
            .entries
            .into_iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| VaultError::new("FILE_NOT_FOUND", "找不到该文件。"))?;
        let cancel = || permit.is_cancelled();
        match file_store::preview_thumbnail(&vault_dir, &entry, &key, &cancel) {
            Err(err) if err.code == "IMPORT_CANCELLED" => {
                Err(VaultError::new("PREVIEW_CANCELLED", "预览已取消。"))
            }
            result => result,
        }
    }

    /// 在内存中解密索引中登记的一个 PDF；明文不落盘，密钥不出后端。
    pub fn preview_pdf(&self, id: String) -> Result<Vec<u8>, VaultError> {
        let (permit, key, vault_dir) = self.begin_task()?;
        let _vault_lock = file_store::VaultLock::acquire_shared(&vault_dir)?;
        let index = file_store::Index::load(&vault_dir, &key)?;
        let entry = index
            .entries
            .into_iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| VaultError::new("FILE_NOT_FOUND", "找不到该文件。"))?;
        let cancel = || permit.is_cancelled();
        match file_store::preview_pdf(&vault_dir, &entry, &key, &cancel) {
            Err(err) if err.code == "IMPORT_CANCELLED" => {
                Err(VaultError::new("PREVIEW_CANCELLED", "预览已取消。"))
            }
            result => result,
        }
    }

    /// 校验库内全部已有文件的完整性。
    pub fn verify_files(&self) -> Result<VerifyReport, VaultError> {
        let (permit, key, vault_dir) = self.begin_task()?;
        let _serial = self.inner.gate.try_serial()?;
        let cancel = || permit.is_cancelled();
        file_store::verify_all(&vault_dir, &key, &cancel)
    }

    /// 把单个文件解密导出到调用方选定的目标路径。
    ///
    /// 只读库内密文，不修改索引，因此不占用导入/校验的串行锁；但仍登记任务许可，
    /// 锁定会取消导出并等待其退出。明文只存在于 Rust 与目标目录的临时文件中。
    pub fn export_file(
        &self,
        id: String,
        target: PathBuf,
        forbidden_dirs: Vec<PathBuf>,
    ) -> Result<ExportedFile, VaultError> {
        let (permit, key, vault_dir) = self.begin_task()?;
        let _vault_lock = file_store::VaultLock::acquire_shared(&vault_dir)?;
        let index = file_store::Index::load(&vault_dir, &key)?;
        let entry = index
            .entries
            .into_iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| VaultError::new("FILE_NOT_FOUND", "找不到该文件。"))?;
        let cancel = || permit.is_cancelled();
        file_store::export_file(
            &vault_dir,
            &entry,
            &key,
            &target,
            &forbidden_dirs,
            &self.inner.app_cache_dir,
            &cancel,
        )
    }

    /// 单文件直接删除。独占许可与库级排他锁覆盖整个事务。
    pub fn delete_file(&self, id: String) -> Result<DeletedFile, VaultError> {
        let (permit, key, vault_dir) = self.begin_exclusive_task()?;
        let _serial = self
            .inner
            .gate
            .try_serial()
            .map_err(|_| VaultError::delete_busy())?;
        let cancel = || permit.is_cancelled();
        file_store::delete_file(&vault_dir, &key, &id, &cancel)
            .map_err(|err| self.recovery_error(err))
    }

    /// FR-32：只改索引中的显示名称，与导入/校验共用串行锁避免并发写索引。
    /// 设置或取消收藏；状态写入加密索引。
    pub fn set_favorite(
        &self,
        id: String,
        favorite: bool,
    ) -> Result<file_store::FileListItem, VaultError> {
        let (_permit, key, vault_dir) = self.begin_task()?;
        file_store::set_favorite(&vault_dir, &key, &id, favorite)
    }

    pub fn rename_file(&self, id: String, name: String) -> Result<FileListItem, VaultError> {
        let (_permit, key, vault_dir) = self.begin_task()?;
        let _serial = self
            .inner
            .gate
            .try_serial()
            .map_err(|_| VaultError::rename_busy())?;
        file_store::rename_file(&vault_dir, &key, &id, &name)
            .map_err(|err| self.recovery_error(err))
    }

    /// FR-42：读取加密索引中的一级文件夹列表。
    pub fn list_folders(&self) -> Result<Vec<file_store::FolderItem>, VaultError> {
        let (_permit, key, vault_dir) = self.begin_task()?;
        file_store::list_folders(&vault_dir, &key)
    }

    /// 创建文件夹；与导入/重命名共用串行入口，避免并发写索引。
    pub fn create_folder(&self, name: String) -> Result<file_store::FolderItem, VaultError> {
        let (_permit, key, vault_dir) = self.begin_task()?;
        let _serial = self
            .inner
            .gate
            .try_serial()
            .map_err(|_| VaultError::folder_busy())?;
        file_store::create_folder(&vault_dir, &key, &name).map_err(|err| self.recovery_error(err))
    }

    /// 重命名文件夹；只改名称，不影响文件归属与收藏。
    pub fn rename_folder(
        &self,
        id: String,
        name: String,
    ) -> Result<file_store::FolderItem, VaultError> {
        let (_permit, key, vault_dir) = self.begin_task()?;
        let _serial = self
            .inner
            .gate
            .try_serial()
            .map_err(|_| VaultError::folder_busy())?;
        file_store::rename_folder(&vault_dir, &key, &id, &name)
            .map_err(|err| self.recovery_error(err))
    }

    /// 删除空文件夹；非空文件夹由 file_store 拒绝。
    pub fn delete_folder(&self, id: String) -> Result<(), VaultError> {
        let (_permit, key, vault_dir) = self.begin_task()?;
        let _serial = self
            .inner
            .gate
            .try_serial()
            .map_err(|_| VaultError::folder_busy())?;
        file_store::delete_folder(&vault_dir, &key, &id).map_err(|err| self.recovery_error(err))
    }

    /// 批量移动文件；`folder_id` 为 None 表示移回根目录。
    pub fn move_files(
        &self,
        ids: Vec<String>,
        folder_id: Option<String>,
    ) -> Result<Vec<file_store::FileListItem>, VaultError> {
        let (_permit, key, vault_dir) = self.begin_task()?;
        let _serial = self
            .inner
            .gate
            .try_serial()
            .map_err(|_| VaultError::folder_busy())?;
        file_store::move_files(&vault_dir, &key, &ids, folder_id.as_deref())
            .map_err(|err| self.recovery_error(err))
    }

    /// FR-44：在共享锁保护下复制保险库必需数据；取消或失败时由备份模块清理结果。
    pub fn backup_vault(
        &self,
        target_root: PathBuf,
        progress: &dyn Fn(backup::BackupProgress),
    ) -> Result<backup::BackupSummary, VaultError> {
        let (permit, key, vault_dir) = self.begin_task()?;
        backup::backup_vault(
            &vault_dir,
            &key,
            &target_root,
            &|| permit.is_cancelled(),
            progress,
        )
    }

    /// FR-46：仅凭备份目录与该备份的主密码，把备份恢复为新的独立保险库。
    ///
    /// 备份不必属于当前保险库，当前会话也无需解锁；密码用备份自身的
    /// `vault.json` 参数验证。当前保险库与原备份保持不变。
    pub fn restore_vault(
        &self,
        backup_dir: PathBuf,
        target_parent: PathBuf,
        password: String,
        progress: &dyn Fn(restore::RestoreProgress),
    ) -> Result<restore::RestoreSummary, VaultError> {
        let permit = self.begin_passive_task()?;
        let password = Zeroizing::new(password);
        let current_vault = self.current_vault_dir();
        restore::restore_backup(
            &backup_dir,
            &target_parent,
            current_vault.as_deref(),
            &password,
            &|| permit.is_cancelled(),
            progress,
        )
    }

    /// FR-46：验证备份目录对应的主密码。
    ///
    /// 只读校验（密码与索引），不创建任何目录或文件；密码不落盘、不写日志。
    pub fn verify_backup_password(
        &self,
        backup_dir: PathBuf,
        password: String,
    ) -> Result<(), VaultError> {
        let password = Zeroizing::new(password);
        restore::verify_backup_password(&backup_dir, &password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tempfile::TempDir;

    const PASSWORD: &str = "correct horse battery staple";

    fn unlocked_session() -> Session {
        let mut session = Session::locked(PathBuf::from("/tmp/haven-test-vault"));
        session.mark_unlocked(
            PathBuf::from("/tmp/haven-test-vault"),
            Zeroizing::new([9u8; 32]),
            "0:c2FsdA==".to_string(),
        );
        session
    }

    #[test]
    fn lock_clears_master_key_and_blocks_key_operations() {
        let mut session = unlocked_session();
        assert!(session.is_unlocked());
        assert!(session.require_master_key().is_ok());

        session.lock();

        assert!(!session.is_unlocked());
        assert!(session.require_master_key().is_err());
        match session.require_master_key() {
            Err(err) => assert_eq!(err.code, "NOT_UNLOCKED"),
            Ok(_) => panic!("锁定后不应提供主密钥"),
        }
        assert_eq!(session.take_status().state, "locked");
    }

    #[test]
    fn status_serialization_does_not_leak_secrets() {
        let mut session = unlocked_session();
        let status = session.take_status();
        let json = serde_json::to_string(&status).expect("serialize");

        assert!(json.contains("\"state\":\"unlocked\""));
        assert!(!json.contains("master"));
        assert!(!json.contains("Master"));
        assert!(!json.contains("key"));
        assert!(!json.contains("Key"));
        assert!(!json.contains("password"));
        assert!(!json.contains("090909"));
    }

    #[test]
    fn location_roundtrip() {
        let temp = TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config").join(CONFIG_FILE);
        let vault_dir = temp.path().join(VAULT_DIR_NAME);

        assert!(read_location(&config_path).expect("read missing").is_none());

        write_location(&config_path, &vault_dir).expect("write location");
        let loaded = read_location(&config_path)
            .expect("read location")
            .expect("some location");
        assert_eq!(loaded, vault_dir);
    }

    #[test]
    fn relative_location_file_is_rejected() {
        let temp = TempDir::new().expect("temp dir");
        let config_path = temp.path().join(CONFIG_FILE);
        fs::write(&config_path, br#"{"vaultDir":"relative/path"}"#).expect("write");

        let err = read_location(&config_path).expect_err("must fail");
        assert_eq!(err.code, "METADATA_INVALID");
    }

    #[test]
    fn corrupt_location_file_is_an_error_not_uninitialized() {
        let temp = TempDir::new().expect("temp dir");
        let config_path = temp.path().join(CONFIG_FILE);
        fs::write(&config_path, b"{ broken").expect("write");

        let err = read_location(&config_path).expect_err("must fail");
        assert_eq!(err.code, "METADATA_INVALID");
    }

    #[test]
    fn bootstrap_opens_existing_vault_as_locked() {
        let temp = TempDir::new().expect("temp dir");
        let (vault_dir, _key) =
            vault::create_vault_dir(temp.path(), PASSWORD).expect("create vault");
        let config_path = temp.path().join(CONFIG_FILE);
        write_location(&config_path, &vault_dir).expect("write location");

        let state = AppState::bootstrap(config_path, temp.path().join("cache"));
        let status = state.status();
        assert_eq!(status.state, "locked");
        assert_eq!(status.vault_path.as_deref(), vault_dir.to_str());
    }

    #[test]
    fn bootstrap_marks_missing_metadata_unavailable_without_recreating() {
        let temp = TempDir::new().expect("temp dir");
        let vault_dir = temp.path().join(VAULT_DIR_NAME);
        fs::create_dir(&vault_dir).expect("dir");
        let config_path = temp.path().join(CONFIG_FILE);
        write_location(&config_path, &vault_dir).expect("write location");

        let state = AppState::bootstrap(config_path, temp.path().join("cache"));
        let status = state.status();
        assert_eq!(status.state, "unavailable");
        assert!(status.message.is_some());
        assert!(!vault::metadata_path(&vault_dir).exists());
    }

    #[test]
    fn create_then_unlock_flow_keeps_key_in_backend() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));

        let created = state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        assert_eq!(created.state, "unlocked");

        let locked = state.lock().expect("lock");
        assert_eq!(locked.state, "locked");

        let wrong = state
            .unlock_vault("wrong password 1234".to_string())
            .expect_err("wrong password must fail");
        assert_eq!(wrong.code, "AUTH_FAILED");

        let unlocked = state.unlock_vault(PASSWORD.to_string()).expect("unlock");
        assert_eq!(unlocked.state, "unlocked");
        assert_eq!(unlocked.vault_path, created.vault_path);

        let json = serde_json::to_string(&unlocked).expect("serialize");
        assert!(!json.contains("090909"));
        assert!(!json.contains("masterKey"));
    }

    #[test]
    fn create_is_rejected_while_unlocked() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));

        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");

        let err = state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect_err("must reject");
        assert_eq!(err.code, "ALREADY_UNLOCKED");
    }

    #[test]
    fn open_rejects_non_vault_directory() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));

        let err = state
            .open_vault(temp.path().to_path_buf())
            .expect_err("must fail");
        assert_eq!(err.code, "METADATA_MISSING");
    }

    #[test]
    fn unlock_without_vault_is_rejected() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));

        let err = state
            .unlock_vault(PASSWORD.to_string())
            .expect_err("must fail");
        assert_eq!(err.code, "VAULT_NOT_FOUND");
    }

    #[test]
    fn import_after_lock_is_rejected() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        state.lock().expect("lock");

        let err = state
            .import_files(vec!["/tmp/whatever.txt".to_string()], &|_| {})
            .expect_err("must reject");
        assert_eq!(err.code, "NOT_UNLOCKED");
        assert_eq!(
            state.list_files().expect_err("must reject").code,
            "NOT_UNLOCKED"
        );
        assert_eq!(
            state.verify_files().expect_err("must reject").code,
            "NOT_UNLOCKED"
        );
    }

    #[test]
    fn idle_cancel_does_not_block_later_imports() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        let source = temp.path().join("later.txt");
        fs::write(&source, b"later").expect("write");

        state.request_cancel();
        let summary = state
            .import_files(vec![source.to_str().expect("utf8").to_string()], &|_| {})
            .expect("import");
        assert_eq!(summary.succeeded.len(), 1);
    }

    #[test]
    fn cancel_only_affects_tasks_started_before_it() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));

        let before = state.inner.gate.begin().expect("permit");
        state.request_cancel();
        assert!(before.is_cancelled());

        // 取消之后才开始的任务不应继承旧的取消代际。
        let after = state.inner.gate.begin().expect("permit");
        assert!(!after.is_cancelled());
    }

    #[test]
    fn lock_timeout_reports_busy_and_keeps_vault_unlocked() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");

        // 模拟一个尚未退出的导入任务持有密钥与任务许可。
        let permit = state.inner.gate.begin().expect("permit");
        let err = state
            .lock_with_timeout(Duration::from_millis(30))
            .expect_err("busy");
        assert_eq!(err.code, "LOCK_BUSY");
        // 失败时不得误报已锁定，也不能清除主密钥。
        assert_eq!(state.status().state, "unlocked");

        drop(permit);
        assert_eq!(state.lock().expect("lock").state, "locked");
    }

    #[test]
    fn shutdown_clears_key_even_with_active_task() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");

        let _permit = state.inner.gate.begin().expect("permit");
        state.shutdown();
        assert_eq!(state.status().state, "locked");
        assert_eq!(state.list_files().expect_err("locked").code, "NOT_UNLOCKED");
    }

    #[test]
    fn preview_after_lock_is_rejected() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        state.lock().expect("lock");

        let err = state
            .preview_image("0000000000000000".to_string())
            .expect_err("locked");
        assert_eq!(err.code, "NOT_UNLOCKED");
    }

    #[test]
    fn preview_pdf_survives_lock_and_reunlock() {
        let temp = TempDir::new().expect("temp dir");
        let source = temp.path().join("preview.pdf");
        let plain = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
        fs::write(&source, plain).expect("write source");
        let source_path = source.to_str().expect("utf8").to_string();

        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        let summary = state
            .import_files(vec![source_path], &|_| {})
            .expect("import");
        let id = summary.succeeded[0].id.clone();

        assert_eq!(state.preview_pdf(id.clone()).expect("preview"), plain);

        state.lock().expect("lock");
        assert_eq!(
            state
                .preview_pdf(id.clone())
                .expect_err("locked preview")
                .code,
            "NOT_UNLOCKED"
        );

        state
            .unlock_vault(PASSWORD.to_string())
            .expect("unlock again");
        assert_eq!(state.preview_pdf(id).expect("preview after unlock"), plain);
    }

    #[test]
    fn preview_unknown_id_is_rejected() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");

        let err = state
            .preview_image("0000000000000000".to_string())
            .expect_err("missing");
        assert_eq!(err.code, "FILE_NOT_FOUND");
    }

    #[test]
    fn import_list_verify_survive_restart_and_unlock() {
        let temp = TempDir::new().expect("temp dir");
        let config_path = temp.path().join(CONFIG_FILE);
        let source = temp.path().join("报告 2026.txt");
        fs::write(&source, b"persisted secret").expect("write source");
        let source_path = source.to_str().expect("utf8").to_string();

        let state = AppState::bootstrap(config_path.clone(), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");

        let summary = state
            .import_files(vec![source_path], &|_| {})
            .expect("import");
        assert_eq!(summary.succeeded.len(), 1);
        assert_eq!(summary.succeeded[0].name, "报告 2026.txt");
        assert_eq!(state.list_files().expect("list").len(), 1);
        assert_eq!(state.verify_files().expect("verify").ok.len(), 1);

        state.lock().expect("lock");
        assert_eq!(state.list_files().expect_err("locked").code, "NOT_UNLOCKED");

        // 模拟完全退出后重新启动。
        drop(state);
        let restarted = AppState::bootstrap(config_path, temp.path().join("cache"));
        assert_eq!(restarted.status().state, "locked");
        restarted
            .unlock_vault(PASSWORD.to_string())
            .expect("unlock after restart");

        let entries = restarted.list_files().expect("list after restart");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "报告 2026.txt");
        assert_eq!(entries[0].size, b"persisted secret".len() as u64);
        let report = restarted.verify_files().expect("verify after restart");
        assert_eq!(report.ok.len(), 1);
        assert!(report.failed.is_empty());
    }

    #[test]
    fn directory_import_survives_restart() {
        let temp = TempDir::new().expect("temp dir");
        let config_path = temp.path().join(CONFIG_FILE);
        let folder = temp.path().join("相册");
        fs::create_dir_all(folder.join("子目录")).expect("dirs");
        fs::write(folder.join("a.txt"), b"a").expect("a");
        fs::write(folder.join("子目录").join("b.txt"), b"b").expect("b");

        let state = AppState::bootstrap(config_path.clone(), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        let summary = state
            .import_files(vec![folder.to_str().expect("utf8").to_string()], &|_| {})
            .expect("import directory");
        assert_eq!(summary.succeeded.len(), 2);
        assert!(summary.failed.is_empty());
        assert_eq!(state.list_files().expect("list").len(), 2);

        drop(state);
        let restarted = AppState::bootstrap(config_path, temp.path().join("cache"));
        restarted
            .unlock_vault(PASSWORD.to_string())
            .expect("unlock after restart");

        let mut names: Vec<String> = restarted
            .list_files()
            .expect("list after restart")
            .into_iter()
            .map(|file| file.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.txt", "b.txt"]);
        let report = restarted.verify_files().expect("verify after restart");
        assert_eq!(report.ok.len(), 2);
        assert!(report.failed.is_empty());
    }

    #[test]
    fn rename_survives_lock_unlock_and_restart() {
        let temp = TempDir::new().expect("temp dir");
        let config_path = temp.path().join(CONFIG_FILE);
        let source = temp.path().join("photo.jpg");
        fs::write(&source, b"rename me").expect("write source");

        let state = AppState::bootstrap(config_path.clone(), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        state
            .import_files(vec![source.to_str().expect("utf8").to_string()], &|_| {})
            .expect("import");
        let id = state.list_files().expect("list")[0].id.clone();

        let renamed = state
            .rename_file(id.clone(), "  假期 照片.jpg ".to_string())
            .expect("rename");
        assert_eq!(renamed.id, id);
        assert_eq!(renamed.name, "假期 照片.jpg");
        assert_eq!(state.list_files().expect("list")[0].name, "假期 照片.jpg");

        // 锁定后拒绝重命名；重新解锁与完全重启后名称仍然有效。
        state.lock().expect("lock");
        assert_eq!(
            state
                .rename_file(id.clone(), "其他.jpg".to_string())
                .expect_err("locked")
                .code,
            "NOT_UNLOCKED"
        );
        state.unlock_vault(PASSWORD.to_string()).expect("unlock");

        drop(state);
        let restarted = AppState::bootstrap(config_path, temp.path().join("cache"));
        restarted
            .unlock_vault(PASSWORD.to_string())
            .expect("unlock after restart");
        let entries = restarted.list_files().expect("list after restart");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "假期 照片.jpg");
        assert_eq!(entries[0].size, b"rename me".len() as u64);
        assert_eq!(restarted.verify_files().expect("verify").ok.len(), 1);
    }

    #[test]
    fn sequential_batch_delete_keeps_index_consistent_across_restart() {
        let temp = TempDir::new().expect("temp dir");
        let config_path = temp.path().join(CONFIG_FILE);
        let mut sources = Vec::new();
        for name in ["one.jpg", "two.pdf", "three.txt"] {
            let path = temp.path().join(name);
            fs::write(&path, name.as_bytes()).expect("write source");
            sources.push(path.to_str().expect("utf8").to_string());
        }

        let state = AppState::bootstrap(config_path.clone(), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        state.import_files(sources, &|_| {}).expect("import");

        let entries = state.list_files().expect("list");
        assert_eq!(entries.len(), 3);
        let doomed: Vec<String> = entries
            .iter()
            .take(2)
            .map(|entry| entry.id.clone())
            .collect();
        // 批量删除由前端逐项调用同一单文件命令，这里按相同顺序验证索引一致性。
        for id in &doomed {
            state.delete_file(id.clone()).expect("batch member delete");
        }

        let remaining = state.list_files().expect("list after batch");
        assert_eq!(remaining.len(), 1);
        assert!(!doomed.contains(&remaining[0].id));
        assert_eq!(state.verify_files().expect("verify").ok.len(), 1);

        drop(state);
        let restarted = AppState::bootstrap(config_path, temp.path().join("cache"));
        restarted
            .unlock_vault(PASSWORD.to_string())
            .expect("unlock after restart");
        let after = restarted.list_files().expect("list after restart");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, remaining[0].id);
        assert_eq!(after[0].name, remaining[0].name);
        assert_eq!(restarted.verify_files().expect("verify").failed.len(), 0);
    }

    #[test]
    fn export_roundtrip_and_guards() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");

        let source = temp.path().join("导出 测试.txt");
        fs::write(&source, b"round trip").expect("write source");
        state
            .import_files(vec![source.to_str().expect("utf8").to_string()], &|_| {})
            .expect("import");
        let id = state.list_files().expect("list")[0].id.clone();

        let target = temp.path().join("导出的结果.txt");
        let exported = state
            .export_file(id.clone(), target.clone(), Vec::new())
            .expect("export");
        assert_eq!(exported.name, "导出 测试.txt");
        assert_eq!(exported.size, b"round trip".len() as u64);
        assert_eq!(fs::read(&target).expect("read"), b"round trip");

        // 目标已存在：拒绝覆盖。
        let err = state
            .export_file(id.clone(), target.clone(), Vec::new())
            .expect_err("must not overwrite");
        assert_eq!(err.code, "FILE_EXISTS");
        assert_eq!(fs::read(&target).expect("read"), b"round trip");

        // 保险库目录内禁止写明文。
        let vault_dir = PathBuf::from(state.status().vault_path.expect("vault path"));
        let err = state
            .export_file(id.clone(), vault_dir.join("leak.txt"), Vec::new())
            .expect_err("vault path");
        assert_eq!(err.code, "EXPORT_TARGET_INVALID");

        // 非法文件 ID。
        let err = state
            .export_file(
                "0000000000000000".to_string(),
                temp.path().join("unknown.txt"),
                Vec::new(),
            )
            .expect_err("unknown id");
        assert_eq!(err.code, "FILE_NOT_FOUND");

        // 锁定后拒绝导出，且不会解密。
        state.lock().expect("lock");
        let err = state
            .export_file(id, temp.path().join("locked.txt"), Vec::new())
            .expect_err("locked");
        assert_eq!(err.code, "NOT_UNLOCKED");
    }

    #[test]
    fn delete_coordinates_with_tasks_lock_and_repeat_requests() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        let source = temp.path().join("delete-session.txt");
        fs::write(&source, b"session delete").expect("source");
        state
            .import_files(vec![source.to_str().expect("utf8").to_string()], &|_| {})
            .expect("import");
        let id = state.list_files().expect("list")[0].id.clone();

        // 所有读任务共用该许可；任一任务活动期间删除必须快速失败。
        let active = state.inner.gate.begin().expect("active task");
        assert_eq!(
            state.delete_file(id.clone()).expect_err("busy").code,
            "DELETE_BUSY"
        );
        drop(active);

        // 独占删除期间，导入、列表、校验、预览、导出和重复删除都不得进入。
        let exclusive = state.inner.gate.begin_exclusive().expect("exclusive");
        assert_eq!(
            state
                .import_files(vec![source.to_str().expect("utf8").to_string()], &|_| {})
                .expect_err("import conflict")
                .code,
            "DELETE_BUSY"
        );
        assert_eq!(
            state.list_files().expect_err("list conflict").code,
            "DELETE_BUSY"
        );
        assert_eq!(
            state.verify_files().expect_err("verify conflict").code,
            "DELETE_BUSY"
        );
        assert_eq!(
            state
                .preview_image(id.clone())
                .expect_err("preview conflict")
                .code,
            "DELETE_BUSY"
        );
        assert_eq!(
            state
                .export_file(id.clone(), temp.path().join("conflict.txt"), Vec::new(),)
                .expect_err("export conflict")
                .code,
            "DELETE_BUSY"
        );
        assert_eq!(
            state
                .delete_file(id.clone())
                .expect_err("repeat conflict")
                .code,
            "DELETE_BUSY"
        );
        assert_eq!(
            state
                .lock_with_timeout(Duration::from_millis(30))
                .expect_err("lock must wait")
                .code,
            "LOCK_BUSY"
        );
        assert_eq!(state.status().state, "unlocked");
        drop(exclusive);

        let deleted = state.delete_file(id.clone()).expect("delete after tasks");
        assert!(!deleted.cleanup_pending);
        assert!(state.list_files().expect("empty list").is_empty());
        assert!(source.exists());
        assert_eq!(
            state.delete_file(id.clone()).expect_err("unknown id").code,
            "FILE_NOT_FOUND"
        );
        state.lock().expect("lock");
        assert_eq!(
            state.delete_file(id).expect_err("locked").code,
            "NOT_UNLOCKED"
        );
    }

    #[test]
    fn delete_unknown_commit_revokes_key_and_recovers_on_reunlock() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        let source = temp.path().join("unknown-delete.txt");
        fs::write(&source, b"unknown delete").expect("source");
        state
            .import_files(vec![source.to_str().expect("utf8").to_string()], &|_| {})
            .expect("import");
        let id = state.list_files().expect("list")[0].id.clone();

        // 真实注入：索引 rename 成功后目录同步失败。
        file_store::fault_injection::set_index_save_fsync_failure(true);
        let err = state.delete_file(id.clone()).expect_err("unknown");
        file_store::fault_injection::set_index_save_fsync_failure(false);

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        // 会话密钥必须被撤销，前端状态回到 locked，正常操作被拒绝。
        assert_eq!(state.status().state, "locked");
        assert_eq!(state.list_files().expect_err("locked").code, "NOT_UNLOCKED");
        assert_eq!(
            state.delete_file(id.clone()).expect_err("locked").code,
            "NOT_UNLOCKED"
        );

        let vault_dir = PathBuf::from(state.status().vault_path.expect("vault path"));
        let staged = vault_dir.join("tmp").join(format!("delete-{id}.hvn"));
        assert!(staged.is_file(), "Unknown 不得删除暂存密文");

        // 重新解锁并按实际（已提交）索引完成恢复清理。
        let status = state.unlock_vault(PASSWORD.to_string()).expect("unlock");
        assert_eq!(status.state, "unlocked");
        assert!(state.list_files().expect("empty list").is_empty());
        assert!(!staged.exists());
    }

    #[test]
    fn import_unknown_commit_revokes_key_and_preserves_file() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        let source = temp.path().join("unknown-import.txt");
        fs::write(&source, b"unknown import").expect("source");

        file_store::fault_injection::set_index_save_fsync_failure(true);
        let err = state
            .import_files(vec![source.to_str().expect("utf8").to_string()], &|_| {})
            .expect_err("unknown");
        file_store::fault_injection::set_index_save_fsync_failure(false);

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert_eq!(state.status().state, "locked");
        assert_eq!(state.list_files().expect_err("locked").code, "NOT_UNLOCKED");

        // 重新解锁：已提交的新密文必须仍在索引中且未被删除。
        state.unlock_vault(PASSWORD.to_string()).expect("unlock");
        let files = state.list_files().expect("list");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "unknown-import.txt");
        let vault_dir = PathBuf::from(state.status().vault_path.expect("vault path"));
        let ciphertext = vault_dir
            .join("files")
            .join(&files[0].id[..2])
            .join(format!("{}.hvn", files[0].id));
        assert!(ciphertext.is_file());
    }

    #[test]
    fn recovery_error_revokes_session_key() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");

        let err = state.recovery_error(VaultError::recovery_required());

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert_eq!(state.status().state, "locked");
        assert_eq!(
            state
                .delete_file("00000000000000000000000000000000".to_string())
                .expect_err("locked")
                .code,
            "NOT_UNLOCKED"
        );
    }

    #[test]
    fn restore_backup_of_other_vault_works_while_locked() {
        let temp = TempDir::new().expect("temp dir");

        // 保险库 A：备份来源，与当前会话无关。
        let parent_a = temp.path().join("a");
        fs::create_dir(&parent_a).expect("parent a");
        let (vault_a, key_a) =
            vault::create_vault_dir(&parent_a, PASSWORD).expect("create vault a");
        file_store::prepare_vault(&vault_a, &key_a).expect("prepare");
        let source = parent_a.join("note.txt");
        fs::write(&source, b"secret note").expect("source");
        file_store::import_batch(
            &vault_a,
            &key_a,
            &[source.to_string_lossy().to_string()],
            &|| false,
            &|_| {},
        )
        .expect("import");

        let backup_root = temp.path().join("backups");
        fs::create_dir(&backup_root).expect("backup root");
        let backup_summary =
            backup::backup_vault(&vault_a, &key_a, &backup_root, &|| false, &|_| {})
                .expect("backup");
        let backup_dir = PathBuf::from(backup_summary.destination);

        // 保险库 B：当前打开但保持锁定，与备份 A 无关。
        let parent_b = temp.path().join("b");
        fs::create_dir(&parent_b).expect("parent b");
        let (vault_b, _key_b) =
            vault::create_vault_dir(&parent_b, "another password 456").expect("create vault b");
        let config_path = temp.path().join(CONFIG_FILE);
        write_location(&config_path, &vault_b).expect("write location");
        let state = AppState::bootstrap(config_path, temp.path().join("cache"));
        assert_eq!(state.status().state, "locked");

        // 锁定状态下仅凭备份自己的主密码完成验证与恢复。
        state
            .verify_backup_password(backup_dir.clone(), PASSWORD.to_string())
            .expect("verify backup password");
        let wrong = state
            .verify_backup_password(backup_dir.clone(), "wrong password 999".to_string())
            .expect_err("wrong password must fail");
        assert_eq!(wrong.code, "RESTORE_PASSWORD_INVALID");

        let target_root = temp.path().join("restore-root");
        fs::create_dir(&target_root).expect("restore root");
        let summary = state
            .restore_vault(
                backup_dir.clone(),
                target_root.clone(),
                PASSWORD.to_string(),
                &|_| {},
            )
            .expect("restore while locked");
        let restored = PathBuf::from(&summary.destination);

        // 恢复后的保险库使用备份自己的主密码解锁，且密文与备份逐字节一致。
        vault::unlock_vault_dir(&restored, PASSWORD).expect("restored unlocks");
        let restored_index = file_store::Index::load(&restored, &key_a).expect("restored index");
        assert_eq!(restored_index.entries.len(), 1);
        let id = &restored_index.entries[0].id;
        let backup_cipher = backup_dir
            .join("files")
            .join(&id[..2])
            .join(format!("{id}.hvn"));
        let restored_cipher = restored
            .join("files")
            .join(&id[..2])
            .join(format!("{id}.hvn"));
        assert_eq!(
            fs::read(&backup_cipher).expect("backup ciphertext"),
            fs::read(&restored_cipher).expect("restored ciphertext")
        );

        // 当前保险库 B 与原备份都保持不变。
        assert!(vault::metadata_path(&vault_b).exists());
        assert!(backup_dir.join(file_store::INDEX_FILE).is_file());
        assert_eq!(fs::read_dir(&target_root).expect("root").count(), 1);
    }

    #[test]
    fn startup_recovery_cleans_orphans_without_unlock() {
        let temp = TempDir::new().expect("temp dir");
        let cache = temp.path().join("cache");
        let staging = cache.join(export_staging::STAGING_DIR_NAME);
        fs::create_dir_all(&staging).expect("staging");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).expect("chmod");
        }
        let token = "0123456789abcdef";
        let sidecar = staging.join(format!(".haven-export-{token}.lock"));
        let partial = staging.join(format!(".haven-export-{token}.part"));
        fs::write(&sidecar, b"").expect("sidecar");
        fs::write(&partial, b"plaintext").expect("partial");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600)).expect("sidecar mode");
            fs::set_permissions(&partial, fs::Permissions::from_mode(0o600)).expect("partial mode");
        }

        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), cache.clone());
        // 未解锁也必须执行启动扫描。
        state.recover_exports();
        assert!(!sidecar.exists());
        assert!(!partial.exists());
        assert_eq!(state.status().state, "uninitialized");
    }

    #[test]
    fn exit_is_idempotent_and_rejects_new_tasks() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));

        // 直接置位退出标志，验证新任务被拒绝（不受解锁状态影响）。
        state.inner.exiting.store(true, Ordering::SeqCst);
        assert_eq!(
            state.list_files().expect_err("must reject").code,
            "APP_EXITING"
        );
        state.inner.exiting.store(false, Ordering::SeqCst);

        let calls = Arc::new(AtomicUsize::new(0));
        let first = Arc::clone(&calls);
        state.begin_exit(Duration::from_millis(10), move || {
            first.fetch_add(1, Ordering::SeqCst);
        });
        // 第二次调用（模拟 CloseRequested 与 ExitRequested 重复触发）不得再启动流程。
        let second = Arc::clone(&calls);
        state.begin_exit(Duration::from_millis(10), move || {
            second.fetch_add(1, Ordering::SeqCst);
        });

        for _ in 0..300 {
            if state.exit_phase() == ExitPhase::Done {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(state.exit_phase(), ExitPhase::Done);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn exit_timeout_still_clears_key() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        {
            let mut session = state
                .inner
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            session.mark_unlocked(
                temp.path().join("vault"),
                Zeroizing::new([7u8; 32]),
                "0:c2FsdA==".to_string(),
            );
        }
        // 模拟一个不可取消的活动任务：退出最多等待超时，随后仍清理主密钥。
        let permit = state.inner.gate.begin().expect("permit");

        state.begin_exit(Duration::from_millis(30), || {});
        for _ in 0..300 {
            if state.exit_phase() == ExitPhase::Done {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(state.exit_phase(), ExitPhase::Done);
        assert!(
            !state
                .inner
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_unlocked()
        );
        drop(permit);
    }

    /// 测试 A：operation 锁被长期占用时，退出必须在有界时间内越过等待。
    #[test]
    fn exit_proceeds_when_operation_lock_is_held() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        {
            let mut session = state.session();
            session.mark_unlocked(
                temp.path().join("vault"),
                Zeroizing::new([3u8; 32]),
                "0:c2FsdA==".to_string(),
            );
        }

        // 测试线程持有 operation，模拟一个无法在预算内结束的业务命令。
        let operation = state.operation();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        state.begin_exit(Duration::from_millis(50), move || {
            let _ = done_tx.send(());
        });

        // exiting 必须在退出线程启动后立即生效，而不是等 operation 释放。
        let exiting_deadline = Instant::now() + Duration::from_secs(2);
        while !state.inner.exiting.load(Ordering::SeqCst) {
            assert!(Instant::now() < exiting_deadline, "exiting 未及时生效");
            std::thread::sleep(Duration::from_millis(5));
        }

        // operation 仍被持有，退出流程也应完成清理而不是无限等待。
        done_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("退出线程不应无限等待 operation");
        assert_eq!(state.exit_phase(), ExitPhase::Done);
        assert!(!state.session().is_unlocked());

        drop(operation);
        assert_eq!(state.session().take_status().state, "locked");
    }

    /// 测试 B：unlock 被挡在提交点之前时，退出清理先完成，unlock 不得写回主密钥。
    #[test]
    fn exit_rejects_unlock_commit_and_does_not_restore_key() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        state.lock().expect("lock");

        // 测试线程持有 operation：解锁线程只能停在最终提交之前。
        let operation = state.operation();

        let unlock_state = state.clone();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let unlock = std::thread::spawn(move || {
            let result = unlock_state.unlock_vault(PASSWORD.to_string());
            let _ = result_tx.send(result.map(|status| status.state));
        });

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        state.begin_exit(Duration::from_millis(50), move || {
            let _ = done_tx.send(());
        });

        // 退出清理（含 session.lock()）必须在解锁线程恢复前完成。
        done_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("退出清理应在有界等待内完成");
        assert!(state.inner.exiting.load(Ordering::SeqCst));

        drop(operation);
        let result = result_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("解锁线程应返回");
        unlock.join().expect("join");
        assert_eq!(
            result.expect_err("退出开始后不得提交解锁结果").code,
            "APP_EXITING"
        );

        let mut session = state.session();
        assert!(!session.is_unlocked(), "退出后主密钥不得写回");
        assert_eq!(session.take_status().state, "locked");
    }

    /// 测试 C：unlock 先完成提交，随后退出仍必须清零主密钥。
    #[test]
    fn exit_clears_key_committed_by_earlier_unlock() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        state.lock().expect("lock");
        let unlocked = state.unlock_vault(PASSWORD.to_string()).expect("unlock");
        assert_eq!(unlocked.state, "unlocked");

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        state.begin_exit(Duration::from_millis(50), move || {
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("退出应在有界时间内完成");

        assert_eq!(state.exit_phase(), ExitPhase::Done);
        assert!(!state.session().is_unlocked());
        assert_eq!(state.status().state, "locked");
    }

    /// 测试 D：退出开始后 create/open 不得提交会话状态，也不得持久化位置。
    #[test]
    fn exit_gate_blocks_create_and_open_commit() {
        // create：拒绝提交，且不创建位置配置。
        let temp = TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config").join(CONFIG_FILE);
        let state = AppState::bootstrap(config_path.clone(), temp.path().join("cache"));
        state.inner.exiting.store(true, Ordering::SeqCst);

        let err = state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect_err("退出中不得提交创建结果");
        assert_eq!(err.code, "APP_EXITING");
        assert!(!config_path.exists(), "退出闸门拒绝后不得写入位置配置");
        assert!(!state.session().is_unlocked());

        // open：待测状态记住保险库 A，退出后尝试打开另一个有效保险库 B。
        // A、B 位于不同 TempDir，任何位置写入或会话切换都会产生可观察差异。
        let temp_a = TempDir::new().expect("temp dir");
        let config_a = temp_a.path().join("config").join(CONFIG_FILE);
        let state_a = AppState::bootstrap(config_a.clone(), temp_a.path().join("cache"));
        let created_a = state_a
            .create_vault(temp_a.path().to_path_buf(), PASSWORD.to_string())
            .expect("create A");
        let vault_a = PathBuf::from(created_a.vault_path.expect("vault path"));
        state_a.lock().expect("lock A");
        let before = fs::read(&config_a).expect("read config");
        assert_eq!(
            read_location(&config_a).expect("read config"),
            Some(vault_a.clone()),
            "准备阶段位置配置应指向 A"
        );

        let temp_b = TempDir::new().expect("temp dir");
        let config_b = temp_b.path().join("config").join(CONFIG_FILE);
        let state_b = AppState::bootstrap(config_b, temp_b.path().join("cache"));
        let created_b = state_b
            .create_vault(temp_b.path().to_path_buf(), PASSWORD.to_string())
            .expect("create B");
        let vault_b = PathBuf::from(created_b.vault_path.expect("vault path"));
        assert_ne!(vault_a, vault_b);

        state_a.inner.exiting.store(true, Ordering::SeqCst);
        let err = state_a
            .open_vault(vault_b)
            .expect_err("退出中不得提交打开结果");
        assert_eq!(err.code, "APP_EXITING");
        assert_eq!(
            fs::read(&config_a).expect("read config"),
            before,
            "退出闸门拒绝后不得重写位置配置"
        );
        assert_eq!(
            read_location(&config_a).expect("read config"),
            Some(vault_a.clone()),
            "位置配置仍应指向 A"
        );
        {
            let session = state_a.session();
            assert_eq!(
                session.vault_dir(),
                Some(vault_a.as_path()),
                "会话不得切换到 B"
            );
            assert!(!session.is_unlocked());
        }
        assert_eq!(state_a.status().state, "locked");
    }

    /// 目录消失：下一次操作失败关闭，保留路径，恢复后可重新解锁。
    #[test]
    fn missing_vault_fails_closed_and_can_reunlock() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        let status = state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        let vault_dir = PathBuf::from(status.vault_path.expect("vault path"));
        state.list_files().expect("正常保险库应可列表");

        let moved = temp.path().join("Haven-offline");
        fs::rename(&vault_dir, &moved).expect("simulate unplug");

        let err = state.list_files().expect_err("目录消失必须失败");
        assert_eq!(err.code, "VAULT_UNAVAILABLE");
        assert!(!state.session().is_unlocked(), "失败关闭必须清空主密钥");
        assert_eq!(
            state.session().vault_dir(),
            Some(vault_dir.as_path()),
            "必须保留路径以便重新解锁"
        );

        fs::rename(&moved, &vault_dir).expect("simulate reconnect");
        let unlocked = state
            .unlock_vault(PASSWORD.to_string())
            .expect("恢复后应可重新解锁");
        assert_eq!(unlocked.state, "unlocked");
        state.list_files().expect("恢复后应可正常列表");
    }

    /// 元数据缺失：读取失败必须失败关闭，不能当作空保险库继续。
    #[test]
    fn missing_metadata_fails_closed() {
        let temp = TempDir::new().expect("temp dir");
        let state = AppState::bootstrap(temp.path().join(CONFIG_FILE), temp.path().join("cache"));
        let status = state
            .create_vault(temp.path().to_path_buf(), PASSWORD.to_string())
            .expect("create");
        let vault_dir = PathBuf::from(status.vault_path.expect("vault path"));
        fs::remove_file(vault::metadata_path(&vault_dir)).expect("remove metadata");

        let err = state.list_files().expect_err("元数据缺失必须失败");
        assert_eq!(err.code, "VAULT_UNAVAILABLE");
        assert!(!state.session().is_unlocked());
        assert_eq!(state.session().vault_dir(), Some(vault_dir.as_path()));
    }

    /// 同一路径换成另一个有效保险库：指纹不一致必须失败关闭。
    #[test]
    fn replaced_vault_fingerprint_fails_closed() {
        let temp_a = TempDir::new().expect("temp dir");
        let state_a =
            AppState::bootstrap(temp_a.path().join(CONFIG_FILE), temp_a.path().join("cache"));
        let created_a = state_a
            .create_vault(temp_a.path().to_path_buf(), PASSWORD.to_string())
            .expect("create A");
        let vault_a = PathBuf::from(created_a.vault_path.expect("vault path"));

        let temp_b = TempDir::new().expect("temp dir");
        let state_b =
            AppState::bootstrap(temp_b.path().join(CONFIG_FILE), temp_b.path().join("cache"));
        let created_b = state_b
            .create_vault(temp_b.path().to_path_buf(), PASSWORD.to_string())
            .expect("create B");
        let vault_b = PathBuf::from(created_b.vault_path.expect("vault path"));

        let aside = temp_a.path().join("Haven-old");
        fs::rename(&vault_a, &aside).expect("move A aside");
        fs::rename(&vault_b, &vault_a).expect("swap B into A path");

        let err = state_a.list_files().expect_err("指纹不一致必须失败");
        assert_eq!(err.code, "VAULT_UNAVAILABLE");
        assert!(!state_a.session().is_unlocked());
        assert_eq!(state_a.session().vault_dir(), Some(vault_a.as_path()));
    }

    /// 进程级 helper：模拟退出状态机在不可取消任务存在时的 2 秒上限。
    #[cfg(unix)]
    #[test]
    fn exit_helper_process() {
        let Ok(cache) = std::env::var("HAVEN_EXIT_HELPER") else {
            return;
        };
        let state = AppState::bootstrap(
            PathBuf::from("/nonexistent-haven-config/vault-location.json"),
            PathBuf::from(cache),
        );
        let permit = state.inner.gate.begin().expect("permit");
        let start = std::time::Instant::now();
        state.begin_exit(Duration::from_secs(2), || {});
        loop {
            if state.exit_phase() == ExitPhase::Done {
                break;
            }
            if start.elapsed() > Duration::from_secs(6) {
                panic!("exit flow did not finish");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("EXITED:{}", start.elapsed().as_millis());
        drop(permit);
    }

    #[cfg(unix)]
    #[test]
    fn exit_process_finishes_within_two_seconds() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let temp = TempDir::new().expect("temp dir");
        let exe = std::env::current_exe().expect("test exe");
        let mut child = Command::new(exe)
            .args([
                "--exact",
                "session::tests::exit_helper_process",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("HAVEN_EXIT_HELPER", temp.path().join("cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn helper");

        let start = std::time::Instant::now();
        let stderr = child.stderr.take().expect("stderr");
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        let mut elapsed: Option<u128> = None;
        loop {
            line.clear();
            if reader.read_line(&mut line).expect("read") == 0 {
                break;
            }
            if let Some(value) = line.trim().strip_prefix("EXITED:") {
                elapsed = value.parse().ok();
                break;
            }
        }
        let status = child.wait().expect("wait");
        assert!(status.success());
        let elapsed = elapsed.expect("exit marker");
        assert!(
            (1800..3500).contains(&elapsed),
            "退出等待应接近 2 秒上限，实际 {elapsed}ms"
        );
        assert!(start.elapsed() < Duration::from_secs(6));
    }
}
