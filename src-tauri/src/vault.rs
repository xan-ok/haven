//! 保险库的加密核心：密码派生、主密钥封装、元数据校验与原子写入。
//!
//! 安全边界：
//! - 主密码只在本模块内以字节切片形式用于 Argon2id，不落盘、不写日志。
//! - 主密钥由操作系统安全随机数生成，只以 `Zeroizing` 包装返回给会话层。
//! - 错误信息不包含文件路径、密码或密钥。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// 保险库目录固定名称（PRD Q-15）。
pub const VAULT_DIR_NAME: &str = "我的保险库";
/// 元数据文件名。
pub const METADATA_FILE: &str = "vault.json";

/// 主密码最少字符数（Unicode 码点）。
pub const PASSWORD_MIN_CHARS: usize = 12;
/// 主密码最多字符数（Unicode 码点），用于阻止异常输入造成的资源消耗。
pub const PASSWORD_MAX_CHARS: usize = 1024;

/// 默认 Argon2id 参数：256 MiB 内存、5 次迭代、单车道。
/// 在本机 release 构建实测约 0.66 秒（见 README 安全说明）。
pub const KDF_M_COST_KIB: u32 = 262_144;
pub const KDF_T_COST: u32 = 5;
pub const KDF_P_COST: u32 = 1;

const METADATA_FORMAT: &str = "haven-vault";
const METADATA_VERSION: u32 = 1;
const KDF_ALGORITHM: &str = "argon2id";
const KDF_VERSION: u32 = 19; // Argon2 v0x13
const CIPHER_ALGORITHM: &str = "xchacha20poly1305";

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const MASTER_KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const CIPHERTEXT_LEN: usize = MASTER_KEY_LEN + TAG_LEN;

const MAX_METADATA_BYTES: u64 = 64 * 1024;
const TEMP_PREFIX: &str = ".vault.json.tmp-";

// 元数据中允许的 KDF 参数范围：防止损坏或被构造的元数据造成无限资源消耗。
const M_COST_MIN: u32 = 65_536;
const M_COST_MAX: u32 = 524_288;
const T_COST_MIN: u32 = 2;
const T_COST_MAX: u32 = 10;
const P_COST_MIN: u32 = 1;
const P_COST_MAX: u32 = 8;

/// 返回给前端的结构化错误。消息中不包含路径、密码或密钥。
#[derive(Debug, Clone, Serialize)]
pub struct VaultError {
    pub code: &'static str,
    pub message: String,
}

impl VaultError {
    pub fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }

    pub fn internal() -> Self {
        Self::new("INTERNAL", "操作失败，请重试。")
    }

    pub fn disk() -> Self {
        Self::new("DISK_ERROR", "无法写入保险库数据，请检查磁盘空间与权限。")
    }

    pub fn auth_failed() -> Self {
        Self::new("AUTH_FAILED", "密码不正确，或保险库数据已损坏。")
    }

    pub fn already_exists() -> Self {
        Self::new(
            "ALREADY_EXISTS",
            "该位置已存在「我的保险库」，Haven 不会覆盖已有保险库。",
        )
    }

    pub fn vault_not_found() -> Self {
        Self::new(
            "VAULT_NOT_FOUND",
            "找不到保险库目录，请确认位置或磁盘是否可用。",
        )
    }

    pub fn metadata_missing() -> Self {
        Self::new("METADATA_MISSING", "该目录中没有找到 Haven 保险库数据。")
    }

    pub fn metadata_invalid() -> Self {
        Self::new("METADATA_INVALID", "保险库数据格式不正确或已损坏。")
    }

    pub fn unsupported_version() -> Self {
        Self::new("UNSUPPORTED_VERSION", "该保险库版本不受当前 Haven 支持。")
    }

    pub fn not_unlocked() -> Self {
        Self::new("NOT_UNLOCKED", "保险库已锁定，请先输入主密码解锁。")
    }

    pub fn vault_unavailable() -> Self {
        Self::new(
            "VAULT_UNAVAILABLE",
            "保险库所在磁盘不可用，请重新连接后解锁。",
        )
    }

    pub fn vault_busy() -> Self {
        Self::new("VAULT_BUSY", "保险库正在被其他操作使用，请稍后重试。")
    }

    pub fn lock_busy() -> Self {
        Self::new("LOCK_BUSY", "正在安全停止当前操作，请稍后再锁定保险库。")
    }

    pub fn import_cancelled() -> Self {
        Self::new("IMPORT_CANCELLED", "导入已取消，未完成的文件已丢弃。")
    }

    pub fn import_in_progress() -> Self {
        Self::new("IMPORT_IN_PROGRESS", "已有导入正在进行，请等待完成或取消。")
    }

    pub fn source_missing() -> Self {
        Self::new("SOURCE_MISSING", "文件不存在或已被移动。")
    }

    pub fn source_unreadable() -> Self {
        Self::new(
            "SOURCE_UNREADABLE",
            "没有读取权限，或文件正在被其他程序使用。",
        )
    }

    pub fn source_unsupported() -> Self {
        Self::new(
            "SOURCE_UNSUPPORTED",
            "只支持导入普通文件；符号链接和特殊文件会被跳过。",
        )
    }

    pub fn source_changed() -> Self {
        Self::new("SOURCE_CHANGED", "文件在导入过程中发生变化，已跳过。")
    }

    pub fn no_space() -> Self {
        Self::new("NO_SPACE", "磁盘空间不足，未完成的文件已丢弃。")
    }

    pub fn file_exists() -> Self {
        Self::new("FILE_EXISTS", "目标文件已存在，Haven 不会覆盖。")
    }

    pub fn export_target_invalid() -> Self {
        Self::new(
            "EXPORT_TARGET_INVALID",
            "导出位置不可用，请重新选择保存位置。",
        )
    }

    pub fn export_protected_path() -> Self {
        Self::new(
            "EXPORT_TARGET_INVALID",
            "不能把明文导出到保险库或应用数据目录，请选择其他位置。",
        )
    }

    pub fn export_write_failed() -> Self {
        Self::new(
            "EXPORT_WRITE_FAILED",
            "无法写入所选位置，请检查磁盘空间与权限。",
        )
    }

    pub fn export_cancelled() -> Self {
        Self::new("EXPORT_CANCELLED", "导出已取消，未完成的临时文件已删除。")
    }

    pub fn export_staging_unavailable() -> Self {
        Self::new(
            "EXPORT_STAGING_UNAVAILABLE",
            "该位置无法安全导出，请选择其他磁盘或文件夹。",
        )
    }

    pub fn export_commit_unknown() -> Self {
        Self::new(
            "EXPORT_COMMIT_UNKNOWN",
            "导出结果未确认，请检查目标位置；应用会在下次启动时清理未完成数据。",
        )
    }

    pub fn export_cleanup_pending() -> Self {
        Self::new(
            "EXPORT_CLEANUP_PENDING",
            "导出未完成，且临时数据清理未完成。Haven 已保留恢复标记，会在下次启动时继续清理。",
        )
    }

    pub fn app_exiting() -> Self {
        Self::new("APP_EXITING", "应用正在退出，请稍后重新启动。")
    }

    pub fn delete_busy() -> Self {
        Self::new("DELETE_BUSY", "其他文件操作尚未结束，请稍后重试。")
    }

    pub fn delete_cancelled() -> Self {
        Self::new("DELETE_CANCELLED", "删除已取消，文件仍保留在 Haven 中。")
    }

    pub fn delete_path_invalid() -> Self {
        Self::new("DELETE_PATH_INVALID", "密文路径异常，删除未执行。")
    }

    pub fn delete_incomplete() -> Self {
        Self::new("DELETE_INCOMPLETE", "删除未完成，文件仍保留在 Haven 中。")
    }

    pub fn rename_busy() -> Self {
        Self::new("RENAME_BUSY", "其他文件操作尚未结束，请稍后重试。")
    }

    pub fn file_name_empty() -> Self {
        Self::new("FILE_NAME_EMPTY", "文件名不能为空。")
    }

    pub fn file_name_too_long() -> Self {
        Self::new("FILE_NAME_TOO_LONG", "文件名不能超过 100 个字符。")
    }

    pub fn file_name_invalid() -> Self {
        Self::new("FILE_NAME_INVALID", "文件名不能包含 / \\ : 等非法字符。")
    }

    pub fn file_name_extension_locked() -> Self {
        Self::new("FILE_NAME_EXTENSION_LOCKED", "重命名需要保留原扩展名。")
    }

    pub fn file_name_exists() -> Self {
        Self::new("FILE_NAME_EXISTS", "已存在同名文件，请换一个名称。")
    }

    pub fn folder_busy() -> Self {
        Self::new("FOLDER_BUSY", "其他文件操作尚未结束，请稍后重试。")
    }

    pub fn folder_name_empty() -> Self {
        Self::new("FOLDER_NAME_EMPTY", "文件夹名称不能为空。")
    }

    pub fn folder_name_too_long() -> Self {
        Self::new("FOLDER_NAME_TOO_LONG", "文件夹名称不能超过 100 个字符。")
    }

    pub fn folder_name_invalid() -> Self {
        Self::new(
            "FOLDER_NAME_INVALID",
            "文件夹名称不能包含 / \\ : 等非法字符。",
        )
    }

    pub fn folder_name_exists() -> Self {
        Self::new("FOLDER_NAME_EXISTS", "已存在同名文件夹，请换一个名称。")
    }

    pub fn folder_not_found() -> Self {
        Self::new("FOLDER_NOT_FOUND", "找不到该文件夹。")
    }

    pub fn folder_not_empty() -> Self {
        Self::new("FOLDER_NOT_EMPTY", "文件夹中还有文件，无法删除。")
    }

    pub fn recovery_required() -> Self {
        Self::new(
            "VAULT_RECOVERY_REQUIRED",
            "保险库数据状态需要恢复，请重新解锁。",
        )
    }

    pub fn integrity() -> Self {
        Self::new("INTEGRITY_ERROR", "文件数据未通过完整性校验。")
    }

    pub fn index_missing() -> Self {
        Self::new(
            "INDEX_MISSING",
            "保险库缺少文件索引，无法确认已有文件，请勿删除库内数据。",
        )
    }

    pub fn index_invalid() -> Self {
        Self::new("INDEX_INVALID", "文件索引已损坏，无法读取。")
    }
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for VaultError {}

/// 保险库元数据。字段顺序与 AAD 拼接顺序一致，便于版本化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultMetadata {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub created_at: u64,
    pub kdf: KdfMetadata,
    pub cipher: CipherMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KdfMetadata {
    pub algorithm: String,
    pub version: u32,
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CipherMetadata {
    pub algorithm: String,
    pub nonce: String,
    pub ciphertext: String,
}

/// 元数据身份指纹：`created_at` 与 KDF salt 的组合。
///
/// 仅用于识别同一路径上的元数据是否被替换，不代表物理磁盘或设备身份。
pub fn identity_fingerprint(metadata: &VaultMetadata) -> String {
    format!("{}:{}", metadata.created_at, metadata.kdf.salt)
}

/// 已完成格式与范围校验的元数据。
pub struct ValidatedMetadata {
    pub salt: [u8; SALT_LEN],
    pub nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
    pub aad: Vec<u8>,
}

/// 统计主密码的字符数。使用 Unicode 码点计数，不做 trim、不做归一化。
pub fn password_char_count(password: &str) -> usize {
    password.chars().count()
}

/// 校验创建保险库时的密码规则。
pub fn validate_password(password: &str) -> Result<(), VaultError> {
    // 先按字节快速拒绝异常输入，避免对超大字符串做完整遍历。
    if password.len() > PASSWORD_MAX_CHARS * 4 {
        return Err(VaultError::new(
            "PASSWORD_TOO_LONG",
            "主密码不能超过 1024 个字符。",
        ));
    }
    let chars = password_char_count(password);
    if chars < PASSWORD_MIN_CHARS {
        return Err(VaultError::new(
            "PASSWORD_TOO_SHORT",
            "主密码至少需要 12 个字符。",
        ));
    }
    if chars > PASSWORD_MAX_CHARS {
        return Err(VaultError::new(
            "PASSWORD_TOO_LONG",
            "主密码不能超过 1024 个字符。",
        ));
    }
    Ok(())
}

/// 元数据文件路径。
pub fn metadata_path(dir: &Path) -> PathBuf {
    dir.join(METADATA_FILE)
}

/// 读取并解析元数据，不做加密校验。
pub fn read_metadata(dir: &Path) -> Result<VaultMetadata, VaultError> {
    if !dir.is_dir() {
        return Err(VaultError::vault_not_found());
    }

    let path = metadata_path(dir);
    let info = match fs::symlink_metadata(&path) {
        Ok(info) => info,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(VaultError::metadata_missing());
        }
        Err(_) => return Err(VaultError::disk()),
    };

    // 拒绝符号链接，避免被诱导读取目录外的文件。
    if info.file_type().is_symlink() || !info.is_file() {
        return Err(VaultError::metadata_invalid());
    }
    if info.len() > MAX_METADATA_BYTES {
        return Err(VaultError::metadata_invalid());
    }

    let file = File::open(&path).map_err(|_| VaultError::disk())?;
    // 校验打开后的文件与打开前的目录项指向同一 inode，缩小符号链接替换的 TOCTOU 窗口。
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata().map_err(|_| VaultError::disk())?;
        if !opened.is_file() || opened.dev() != info.dev() || opened.ino() != info.ino() {
            return Err(VaultError::metadata_invalid());
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| VaultError::disk())?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(VaultError::metadata_invalid());
    }

    serde_json::from_slice(&bytes).map_err(|_| VaultError::metadata_invalid())
}

/// 校验元数据的格式、版本、长度与参数范围。
pub fn validate_metadata(metadata: &VaultMetadata) -> Result<ValidatedMetadata, VaultError> {
    if metadata.format != METADATA_FORMAT {
        return Err(VaultError::metadata_invalid());
    }
    if metadata.version != METADATA_VERSION {
        return Err(VaultError::unsupported_version());
    }
    if metadata.name != VAULT_DIR_NAME {
        return Err(VaultError::metadata_invalid());
    }
    if metadata.created_at == 0 {
        return Err(VaultError::metadata_invalid());
    }
    if metadata.kdf.algorithm != KDF_ALGORITHM {
        return Err(VaultError::metadata_invalid());
    }
    if metadata.kdf.version != KDF_VERSION {
        return Err(VaultError::unsupported_version());
    }
    if !(M_COST_MIN..=M_COST_MAX).contains(&metadata.kdf.m_cost_kib)
        || !(T_COST_MIN..=T_COST_MAX).contains(&metadata.kdf.t_cost)
        || !(P_COST_MIN..=P_COST_MAX).contains(&metadata.kdf.p_cost)
    {
        return Err(VaultError::metadata_invalid());
    }
    if metadata.cipher.algorithm != CIPHER_ALGORITHM {
        return Err(VaultError::metadata_invalid());
    }

    let salt_bytes = STANDARD
        .decode(metadata.kdf.salt.as_bytes())
        .map_err(|_| VaultError::metadata_invalid())?;
    let salt: [u8; SALT_LEN] = salt_bytes
        .try_into()
        .map_err(|_| VaultError::metadata_invalid())?;

    let nonce_bytes = STANDARD
        .decode(metadata.cipher.nonce.as_bytes())
        .map_err(|_| VaultError::metadata_invalid())?;
    let nonce: [u8; NONCE_LEN] = nonce_bytes
        .try_into()
        .map_err(|_| VaultError::metadata_invalid())?;

    let ciphertext = STANDARD
        .decode(metadata.cipher.ciphertext.as_bytes())
        .map_err(|_| VaultError::metadata_invalid())?;
    if ciphertext.len() != CIPHERTEXT_LEN {
        return Err(VaultError::metadata_invalid());
    }

    let aad = build_aad(metadata, &salt, &nonce);

    Ok(ValidatedMetadata {
        salt,
        nonce,
        ciphertext,
        aad,
    })
}

/// 认证附加数据：把所有未加密的元数据字段纳入认证，任何篡改都会导致解密失败。
/// salt 与 nonce 在解码后重新按标准 base64 编码，避免编码差异影响认证。
fn build_aad(metadata: &VaultMetadata, salt: &[u8; SALT_LEN], nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        metadata.format,
        metadata.version,
        metadata.name,
        metadata.created_at,
        metadata.kdf.algorithm,
        metadata.kdf.version,
        metadata.kdf.m_cost_kib,
        metadata.kdf.t_cost,
        metadata.kdf.p_cost,
        STANDARD.encode(salt),
        metadata.cipher.algorithm,
        STANDARD.encode(nonce),
    )
    .into_bytes()
}

fn derive_key(
    password: &str,
    salt: &[u8],
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, VaultError> {
    let params = Params::new(m_cost_kib, t_cost, p_cost, Some(MASTER_KEY_LEN))
        .map_err(|_| VaultError::metadata_invalid())?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut derived = Zeroizing::new([0u8; MASTER_KEY_LEN]);
    argon
        .hash_password_into(password.as_bytes(), salt, &mut derived[..])
        .map_err(|_| VaultError::internal())?;
    Ok(derived)
}

fn encrypt_master_key(
    derived: &[u8],
    nonce: &[u8; NONCE_LEN],
    master_key: &[u8; MASTER_KEY_LEN],
    aad: &[u8],
) -> Result<Vec<u8>, VaultError> {
    let cipher = XChaCha20Poly1305::new_from_slice(derived).map_err(|_| VaultError::internal())?;
    let nonce = XNonce::try_from(&nonce[..]).map_err(|_| VaultError::internal())?;
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: master_key,
                aad,
            },
        )
        .map_err(|_| VaultError::internal())
}

fn decrypt_master_key(
    derived: &[u8],
    nonce: &[u8; NONCE_LEN],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, VaultError> {
    let cipher = XChaCha20Poly1305::new_from_slice(derived).map_err(|_| VaultError::internal())?;
    let nonce = XNonce::try_from(&nonce[..]).map_err(|_| VaultError::internal())?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| VaultError::auth_failed())?,
    );
    if plaintext.len() != MASTER_KEY_LEN {
        return Err(VaultError::auth_failed());
    }
    let mut master_key = Zeroizing::new([0u8; MASTER_KEY_LEN]);
    master_key.copy_from_slice(&plaintext);
    Ok(master_key)
}

pub(crate) fn unix_now() -> Result<u64, VaultError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| VaultError::internal())
}

pub(crate) fn random_suffix() -> Result<String, VaultError> {
    let bytes = <[u8; 8]>::try_generate().map_err(|_| VaultError::internal())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(unix)]
fn create_dir_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_dir_private(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

fn open_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// 在目录内以临时文件 + rename 的方式原子写入元数据。
///
/// 写入前显式检查目标是否已存在，避免静默覆盖已有 `vault.json`。
/// 使用 rename 而不是硬链接，以兼容 exFAT/FAT32 等不支持硬链接的外置磁盘。
fn write_metadata_atomic(dir: &Path, metadata: &VaultMetadata) -> Result<(), VaultError> {
    let bytes = serde_json::to_vec_pretty(metadata).map_err(|_| VaultError::internal())?;
    let temp_path = dir.join(format!("{TEMP_PREFIX}{}", random_suffix()?));
    let metadata_path = metadata_path(dir);

    let mut file = open_private_file(&temp_path).map_err(|_| VaultError::disk())?;
    if file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(&temp_path);
        return Err(VaultError::disk());
    }
    drop(file);

    // symlink_metadata 能发现悬空符号链接等任何已存在的目录项。
    if fs::symlink_metadata(&metadata_path).is_ok() {
        let _ = fs::remove_file(&temp_path);
        return Err(VaultError::already_exists());
    }
    if fs::rename(&temp_path, &metadata_path).is_err() {
        let _ = fs::remove_file(&temp_path);
        return Err(VaultError::disk());
    }

    // 目录 fsync 属于尽力而为：失败不代表元数据无效，不回滚。
    let _ = File::open(dir).and_then(|handle| handle.sync_all());
    Ok(())
}

fn init_vault_dir(
    dir: &Path,
    password: &str,
) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, VaultError> {
    let master_key =
        Zeroizing::new(<[u8; MASTER_KEY_LEN]>::try_generate().map_err(|_| VaultError::internal())?);
    let salt = <[u8; SALT_LEN]>::try_generate().map_err(|_| VaultError::internal())?;
    let nonce = <[u8; NONCE_LEN]>::try_generate().map_err(|_| VaultError::internal())?;
    let created_at = unix_now()?;

    let mut metadata = VaultMetadata {
        format: METADATA_FORMAT.to_string(),
        version: METADATA_VERSION,
        name: VAULT_DIR_NAME.to_string(),
        created_at,
        kdf: KdfMetadata {
            algorithm: KDF_ALGORITHM.to_string(),
            version: KDF_VERSION,
            m_cost_kib: KDF_M_COST_KIB,
            t_cost: KDF_T_COST,
            p_cost: KDF_P_COST,
            salt: STANDARD.encode(salt),
        },
        cipher: CipherMetadata {
            algorithm: CIPHER_ALGORITHM.to_string(),
            nonce: STANDARD.encode(nonce),
            ciphertext: String::new(),
        },
    };

    let derived = derive_key(password, &salt, KDF_M_COST_KIB, KDF_T_COST, KDF_P_COST)?;
    let aad = build_aad(&metadata, &salt, &nonce);
    let ciphertext = encrypt_master_key(&derived[..], &nonce, &master_key, &aad)?;
    metadata.cipher.ciphertext = STANDARD.encode(ciphertext);

    write_metadata_atomic(dir, &metadata)?;
    Ok(master_key)
}

/// 创建保险库：在 `parent` 下创建固定名称的子目录，初始化元数据。
///
/// 成功时返回保险库目录与主密钥；失败时不留下可被误认为有效保险库的目录。
pub fn create_vault_dir(
    parent: &Path,
    password: &str,
) -> Result<(PathBuf, Zeroizing<[u8; MASTER_KEY_LEN]>), VaultError> {
    validate_password(password)?;

    if !parent.is_dir() {
        return Err(VaultError::new(
            "PATH_INVALID",
            "所选位置不可用，请确认目录存在且磁盘已连接。",
        ));
    }

    let vault_dir = parent.join(VAULT_DIR_NAME);
    match fs::symlink_metadata(&vault_dir) {
        Ok(_) => return Err(VaultError::already_exists()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(VaultError::disk()),
    }

    create_dir_private(&vault_dir).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            VaultError::already_exists()
        } else {
            VaultError::disk()
        }
    })?;

    match init_vault_dir(&vault_dir, password) {
        Ok(master_key) => Ok((vault_dir, master_key)),
        Err(err) => {
            // 该目录由本次调用创建；失败时整体移除，避免留下半成品。
            let _ = fs::remove_dir_all(&vault_dir);
            Err(err)
        }
    }
}

/// 打开并解锁已有保险库。只有认证成功才返回主密钥。
pub fn unlock_vault_dir(
    dir: &Path,
    password: &str,
) -> Result<Zeroizing<[u8; MASTER_KEY_LEN]>, VaultError> {
    let metadata = read_metadata(dir)?;
    let validated = validate_metadata(&metadata)?;

    // 创建时已强制最少 12 个字符，更短的输入不可能正确；直接拒绝，避免无意义的派生开销。
    let chars = password_char_count(password);
    if !(PASSWORD_MIN_CHARS..=PASSWORD_MAX_CHARS).contains(&chars) {
        return Err(VaultError::auth_failed());
    }

    let derived = derive_key(
        password,
        &validated.salt,
        metadata.kdf.m_cost_kib,
        metadata.kdf.t_cost,
        metadata.kdf.p_cost,
    )?;
    decrypt_master_key(
        &derived[..],
        &validated.nonce,
        &validated.ciphertext,
        &validated.aad,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::time::Instant;
    use tempfile::TempDir;

    const PASSWORD: &str = "correct horse battery staple";

    fn create_sample() -> (TempDir, PathBuf) {
        let temp = TempDir::new().expect("temp dir");
        let (dir, _key) =
            create_vault_dir(temp.path(), PASSWORD).expect("create vault should succeed");
        (temp, dir)
    }

    fn read_json(dir: &Path) -> Value {
        let bytes = fs::read(metadata_path(dir)).expect("read metadata");
        serde_json::from_slice(&bytes).expect("parse metadata")
    }

    fn write_json(dir: &Path, value: &Value) {
        let bytes = serde_json::to_vec_pretty(value).expect("serialize metadata");
        fs::write(metadata_path(dir), bytes).expect("write metadata");
    }

    #[test]
    fn creates_vault_with_fixed_name_and_valid_metadata() {
        let (temp, dir) = create_sample();
        assert_eq!(dir, temp.path().join(VAULT_DIR_NAME));
        assert!(dir.is_dir());

        let metadata = read_metadata(&dir).expect("metadata");
        assert_eq!(metadata.format, METADATA_FORMAT);
        assert_eq!(metadata.version, 1);
        assert_eq!(metadata.name, VAULT_DIR_NAME);
        assert_eq!(metadata.kdf.algorithm, KDF_ALGORITHM);
        assert_eq!(metadata.kdf.m_cost_kib, KDF_M_COST_KIB);
        assert_eq!(metadata.kdf.t_cost, KDF_T_COST);
        assert_eq!(metadata.kdf.p_cost, KDF_P_COST);
        validate_metadata(&metadata).expect("metadata should validate");
    }

    #[test]
    fn correct_password_unlocks() {
        let (_temp, dir) = create_sample();
        unlock_vault_dir(&dir, PASSWORD).expect("correct password should unlock");
    }

    #[test]
    fn wrong_password_is_rejected() {
        let (_temp, dir) = create_sample();
        let err = unlock_vault_dir(&dir, "wrong password 1234").expect_err("must fail");
        assert_eq!(err.code, "AUTH_FAILED");
    }

    #[test]
    fn password_is_not_trimmed_or_normalized() {
        let temp = TempDir::new().expect("temp dir");
        let password = "  spaced password  ";
        let (dir, _key) = create_vault_dir(temp.path(), password).expect("create");
        let err = unlock_vault_dir(&dir, password.trim()).expect_err("trimmed must fail");
        assert_eq!(err.code, "AUTH_FAILED");
        unlock_vault_dir(&dir, password).expect("exact password should unlock");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let (_temp, dir) = create_sample();
        let mut value = read_json(&dir);
        let ciphertext = value["cipher"]["ciphertext"]
            .as_str()
            .expect("ciphertext")
            .to_string();
        let mut chars: Vec<char> = ciphertext.chars().collect();
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        value["cipher"]["ciphertext"] = Value::String(chars.into_iter().collect());

        write_json(&dir, &value);
        let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "AUTH_FAILED");
    }

    #[test]
    fn tampered_created_at_is_rejected() {
        let (_temp, dir) = create_sample();
        let mut value = read_json(&dir);
        let created_at = value["createdAt"].as_u64().expect("createdAt");
        value["createdAt"] = Value::from(created_at + 1);

        write_json(&dir, &value);
        let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "AUTH_FAILED");
    }

    #[test]
    fn tampered_name_is_rejected() {
        let (_temp, dir) = create_sample();
        let mut value = read_json(&dir);
        value["name"] = Value::String("另一个保险库".to_string());

        write_json(&dir, &value);
        let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "METADATA_INVALID");
    }

    #[test]
    fn tampered_kdf_parameters_are_rejected() {
        let (_temp, dir) = create_sample();
        let mut value = read_json(&dir);
        value["kdf"]["mCostKib"] = Value::from(131_072);

        write_json(&dir, &value);
        let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "AUTH_FAILED");
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let (_temp, dir) = create_sample();
        let mut value = read_json(&dir);
        value["version"] = Value::from(2);

        write_json(&dir, &value);
        let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "UNSUPPORTED_VERSION");
    }

    #[test]
    fn metadata_missing_is_not_recreated() {
        let temp = TempDir::new().expect("temp dir");
        let dir = temp.path().join(VAULT_DIR_NAME);
        fs::create_dir(&dir).expect("dir");

        let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "METADATA_MISSING");
        assert!(!metadata_path(&dir).exists());
        assert_eq!(fs::read_dir(&dir).expect("read dir").count(), 0);
    }

    #[test]
    fn leftover_temp_file_is_not_a_valid_vault() {
        let temp = TempDir::new().expect("temp dir");
        let dir = temp.path().join(VAULT_DIR_NAME);
        fs::create_dir(&dir).expect("dir");
        fs::write(dir.join(format!("{TEMP_PREFIX}abcd1234")), b"partial").expect("temp file");

        let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "METADATA_MISSING");
    }

    #[test]
    fn existing_vault_is_never_overwritten() {
        let temp = TempDir::new().expect("temp dir");
        let dir = temp.path().join(VAULT_DIR_NAME);
        fs::create_dir(&dir).expect("dir");
        let keep = dir.join("keep.txt");
        fs::write(&keep, b"user data").expect("write keep");

        let err = create_vault_dir(temp.path(), PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "ALREADY_EXISTS");
        assert_eq!(fs::read(&keep).expect("read keep"), b"user data");
        assert!(!metadata_path(&dir).exists());
    }

    #[test]
    fn malformed_metadata_is_rejected() {
        let temp = TempDir::new().expect("temp dir");
        let dir = temp.path().join(VAULT_DIR_NAME);
        fs::create_dir(&dir).expect("dir");
        fs::write(metadata_path(&dir), b"{ not json").expect("write malformed");

        let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "METADATA_INVALID");
    }

    #[test]
    fn oversized_metadata_is_rejected() {
        let temp = TempDir::new().expect("temp dir");
        let dir = temp.path().join(VAULT_DIR_NAME);
        fs::create_dir(&dir).expect("dir");
        fs::write(
            metadata_path(&dir),
            vec![b'x'; MAX_METADATA_BYTES as usize + 1],
        )
        .expect("write oversized");

        let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "METADATA_INVALID");
    }

    #[test]
    fn invalid_base64_and_lengths_are_rejected() {
        let (_temp, dir) = create_sample();

        let mut value = read_json(&dir);
        value["kdf"]["salt"] = Value::String("!!!!".to_string());
        write_json(&dir, &value);
        assert_eq!(
            unlock_vault_dir(&dir, PASSWORD)
                .expect_err("must fail")
                .code,
            "METADATA_INVALID"
        );

        let mut value = read_json(&dir);
        value["kdf"]["salt"] = Value::String(STANDARD.encode([0u8; 15]));
        write_json(&dir, &value);
        assert_eq!(
            unlock_vault_dir(&dir, PASSWORD)
                .expect_err("must fail")
                .code,
            "METADATA_INVALID"
        );

        let mut value = read_json(&dir);
        value["cipher"]["nonce"] = Value::String(STANDARD.encode([0u8; 23]));
        write_json(&dir, &value);
        assert_eq!(
            unlock_vault_dir(&dir, PASSWORD)
                .expect_err("must fail")
                .code,
            "METADATA_INVALID"
        );

        let mut value = read_json(&dir);
        value["cipher"]["ciphertext"] = Value::String(STANDARD.encode([0u8; 47]));
        write_json(&dir, &value);
        assert_eq!(
            unlock_vault_dir(&dir, PASSWORD)
                .expect_err("must fail")
                .code,
            "METADATA_INVALID"
        );
    }

    #[test]
    fn out_of_range_kdf_parameters_are_rejected_before_derivation() {
        let (_temp, dir) = create_sample();

        for (field, value) in [
            ("mCostKib", 1024u64),
            ("mCostKib", 524_289),
            ("tCost", 1),
            ("tCost", 11),
            ("pCost", 0),
            ("pCost", 9),
        ] {
            let mut json = read_json(&dir);
            json["kdf"][field] = Value::from(value);
            write_json(&dir, &json);

            let start = Instant::now();
            let err = unlock_vault_dir(&dir, PASSWORD).expect_err("must fail");
            assert_eq!(err.code, "METADATA_INVALID", "field={field} value={value}");
            assert!(
                start.elapsed() < std::time::Duration::from_millis(200),
                "参数越界应在派生之前被拒绝"
            );
        }
    }

    #[test]
    fn rejects_unknown_format_and_algorithm() {
        let (_temp, dir) = create_sample();

        let mut value = read_json(&dir);
        value["format"] = Value::String("other-vault".to_string());
        write_json(&dir, &value);
        assert_eq!(
            unlock_vault_dir(&dir, PASSWORD)
                .expect_err("must fail")
                .code,
            "METADATA_INVALID"
        );

        let mut value = read_json(&dir);
        value["cipher"]["algorithm"] = Value::String("aes-gcm".to_string());
        write_json(&dir, &value);
        assert_eq!(
            unlock_vault_dir(&dir, PASSWORD)
                .expect_err("must fail")
                .code,
            "METADATA_INVALID"
        );
    }

    #[test]
    fn password_rule_boundaries() {
        assert!(validate_password("").is_err());
        assert_eq!(
            validate_password("12345678901").expect_err("11 chars").code,
            "PASSWORD_TOO_SHORT"
        );
        validate_password("123456789012").expect("12 chars should pass");
        validate_password("            ").expect("12 spaces are valid");

        let max = "a".repeat(PASSWORD_MAX_CHARS);
        validate_password(&max).expect("1024 chars should pass");

        let too_long = "a".repeat(PASSWORD_MAX_CHARS + 1);
        assert_eq!(
            validate_password(&too_long).expect_err("too long").code,
            "PASSWORD_TOO_LONG"
        );
        assert_eq!(password_char_count("😀😀😀😀😀😀😀😀😀😀😀😀"), 12);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unavailable_external_disk_path_is_rejected() {
        let parent = Path::new("/Volumes/haven-test-unmounted-volume-does-not-exist");
        let err = create_vault_dir(parent, PASSWORD).expect_err("must fail");
        assert_eq!(err.code, "PATH_INVALID");
    }

    #[test]
    #[ignore = "手动运行的 Argon2id 性能基准"]
    fn benchmark_argon2id() {
        let salt = [7u8; SALT_LEN];
        let password = "benchmark password 2026";
        let start = Instant::now();
        let key =
            derive_key(password, &salt, KDF_M_COST_KIB, KDF_T_COST, KDF_P_COST).expect("derive");
        println!(
            "argon2id m={KDF_M_COST_KIB} KiB t={KDF_T_COST} p={KDF_P_COST}: {:?}",
            start.elapsed()
        );
        std::hint::black_box(&key);
    }
}
