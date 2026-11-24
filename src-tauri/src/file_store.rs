//! 加密文件存储：分块 AEAD 文件格式、加密索引与导入事务。
//!
//! 安全边界：
//! - 本模块只接收主密钥的不可变引用，不持有会话，不向前端暴露密钥。
//! - 每个文件使用由主密钥与随机 `file_id` 经 HKDF-SHA256 派生的独立子密钥。
//! - 分块使用 RustCrypto `aead-stream` 的 STREAM（BE32）构造，块序号与最后一块标志
//!   由 STREAM 写入 nonce，文件头与块大小等元数据通过 AAD 认证。
//! - 索引中的文件名、大小与摘要只存在于加密索引内，库目录中只有随机 ID 命名的密文。

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use aead_stream::{DecryptorBE32, EncryptorBE32, StreamBE32};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::export_staging::{ExportTarget, StagingDir};
use crate::export_txn::{ExportCommit, ExportTxn};
use crate::vault::{VaultError, random_suffix, unix_now};

pub const FILES_DIR: &str = "files";
pub const TMP_DIR: &str = "tmp";
pub const INDEX_FILE: &str = "index.hvn";

const FILE_MAGIC: &[u8; 4] = b"HVN1";
const FILE_VERSION: u16 = 1;
const HEADER_LEN: usize = 64;
const FILE_ID_LEN: usize = 16;
const STREAM_NONCE_PREFIX_LEN: usize = 24 - 5;

const INDEX_MAGIC: &[u8; 4] = b"HVI1";
const INDEX_VERSION: u32 = 1;
const INDEX_HEADER_LEN: usize = 56;
const INDEX_SALT_LEN: usize = 16;
const INDEX_NONCE_LEN: usize = 24;
const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;

const CHUNK_SIZE: usize = 1024 * 1024;
const CHUNK_SIZE_MIN: u32 = 64 * 1024;
const CHUNK_SIZE_MAX: u32 = 8 * 1024 * 1024;

/// FR-32 重命名的显示名称上限，与前端保持一致。
pub const RENAME_NAME_MAX_CHARS: usize = 100;
/// FR-42 文件夹名称上限，与文件显示名称保持一致。
pub const FOLDER_NAME_MAX_CHARS: usize = 100;
const TAG_LEN: usize = 16;

const FILE_AAD_PREFIX: &[u8] = b"haven/file/v1\0";
const INDEX_AAD_PREFIX: &[u8] = b"haven/index/v1\0";
const FILE_KEY_INFO: &[u8] = b"haven/file-key/v1";
const INDEX_KEY_INFO: &[u8] = b"haven/index-key/v1";

type StreamNonce = aead_stream::Nonce<XChaCha20Poly1305, StreamBE32<XChaCha20Poly1305>>;
type StreamEncryptor = EncryptorBE32<XChaCha20Poly1305>;
type StreamDecryptor = DecryptorBE32<XChaCha20Poly1305>;

/// 加密索引中的单个条目。所有字段都位于加密索引内。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub chunk_size: u32,
    pub imported_at: u64,
    pub sha256: String,
    /// FR-41 收藏标记；旧索引缺失该字段时按未收藏读取。
    #[serde(default)]
    pub favorite: bool,
    /// FR-42 所属文件夹 ID；缺失或为 null 表示根目录。旧索引向后兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
}

/// 加密索引中的文件夹条目。只保存稳定 ID 与显示名称。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderEntry {
    pub id: String,
    pub name: String,
}

/// 返回给前端的文件夹条目，不包含任何文件内容或路径。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderItem {
    pub id: String,
    pub name: String,
}

impl From<FolderEntry> for FolderItem {
    fn from(entry: FolderEntry) -> Self {
        Self {
            id: entry.id,
            name: entry.name,
        }
    }
}

/// 返回给前端的文件条目。刻意不包含明文 SHA-256，减少元数据暴露面。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileListItem {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub imported_at: u64,
    pub favorite: bool,
    /// 始终返回（根目录为 null），便于前端在移动后准确更新归属。
    pub folder_id: Option<String>,
}

impl From<IndexEntry> for FileListItem {
    fn from(entry: IndexEntry) -> Self {
        Self {
            id: entry.id,
            name: entry.name,
            size: entry.size,
            imported_at: entry.imported_at,
            favorite: entry.favorite,
            folder_id: entry.folder_id,
        }
    }
}

/// 导出结果只返回名称与大小，不包含明文、路径或摘要。
/// `warning` 只在“文件已生成但持久化未确认”时出现，文案固定且不含路径。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedFile {
    pub name: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<&'static str>,
}

/// 删除已提交后，物理清理可能在下次解锁时继续。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedFile {
    pub cleanup_pending: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    format: String,
    version: u32,
    entries: Vec<IndexEntry>,
    /// FR-42 一级文件夹；旧索引缺失该字段时按空列表读取。
    #[serde(default)]
    folders: Vec<FolderEntry>,
}

#[derive(Debug, Default)]
pub struct Index {
    pub entries: Vec<IndexEntry>,
    pub folders: Vec<FolderEntry>,
}

/// 索引原子替换的三态结果。`Unknown` 表示 rename 后同步失败，不能猜测新旧索引。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexCommit {
    NotCommitted,
    Committed,
    Unknown,
}

/// 仅测试构建启用的可控故障注入：模拟索引原子 rename 成功后目录同步失败。
///
/// 该模块只在 `cfg(test)` 下编译，生产构建中不存在任何触发入口。
#[cfg(test)]
pub(crate) mod fault_injection {
    use std::cell::Cell;

    thread_local! {
        static INDEX_SAVE_FSYNC_FAILURE: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn set_index_save_fsync_failure(enabled: bool) {
        INDEX_SAVE_FSYNC_FAILURE.with(|flag| flag.set(enabled));
    }

    pub(crate) fn index_save_fsync_failure() -> bool {
        INDEX_SAVE_FSYNC_FAILURE.with(|flag| flag.get())
    }
}

#[derive(Debug)]
struct IndexSaveError {
    commit: IndexCommit,
    error: VaultError,
}

impl IndexSaveError {
    fn not_committed(error: VaultError) -> Self {
        Self {
            commit: IndexCommit::NotCommitted,
            error,
        }
    }

    fn unknown(error: VaultError) -> Self {
        Self {
            commit: IndexCommit::Unknown,
            error,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedFile {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub renamed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedFile {
    pub name: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub succeeded: Vec<ImportedFile>,
    pub failed: Vec<FailedFile>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub index: usize,
    pub total: usize,
    pub name: String,
    pub phase: &'static str,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyItem {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyFailure {
    pub id: String,
    pub name: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub checked: usize,
    pub ok: Vec<VerifyItem>,
    pub failed: Vec<VerifyFailure>,
    pub cancelled: bool,
}

struct FileHeader {
    file_id: [u8; FILE_ID_LEN],
    nonce_prefix: [u8; STREAM_NONCE_PREFIX_LEN],
    original_size: u64,
    chunk_size: u32,
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode_16(text: &str) -> Option<[u8; FILE_ID_LEN]> {
    if text.len() != FILE_ID_LEN * 2 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = [0u8; FILE_ID_LEN];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out[index] = ((high << 4) | low) as u8;
    }
    Some(out)
}

fn chunk_count(size: u64, chunk_size: u32) -> u64 {
    if size == 0 {
        1
    } else {
        size.div_ceil(chunk_size as u64)
    }
}

fn valid_chunk_layout(size: u64, chunk_size: u32) -> bool {
    (CHUNK_SIZE_MIN..=CHUNK_SIZE_MAX).contains(&chunk_size)
        && chunk_count(size, chunk_size) < u32::MAX as u64
}

fn derive_key(master_key: &[u8; 32], salt: &[u8], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), master_key);
    let mut okm = Zeroizing::new([0u8; 32]);
    // 32 字节输出远小于 HKDF-SHA256 上限，expand 不会失败。
    let _ = hkdf.expand(info, okm.as_mut());
    okm
}

fn file_key(master_key: &[u8; 32], file_id: &[u8; FILE_ID_LEN]) -> Zeroizing<[u8; 32]> {
    derive_key(master_key, file_id, FILE_KEY_INFO)
}

/// 构造 AEAD 实例后立即清零临时密钥数组；密钥已复制进 cipher 状态。
fn make_cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    let mut raw = Key::default();
    raw.copy_from_slice(key);
    let cipher = XChaCha20Poly1305::new(&raw);
    // 用 zeroize 的易失写入清零临时副本，避免编译器把清零当作无效写入删除。
    raw.as_mut_slice().zeroize();
    cipher
}

fn make_encryptor(key: &[u8; 32], nonce: &StreamNonce) -> StreamEncryptor {
    let mut raw = Key::default();
    raw.copy_from_slice(key);
    let encryptor = EncryptorBE32::new(&raw, nonce);
    raw.as_mut_slice().zeroize();
    encryptor
}

fn make_decryptor(key: &[u8; 32], nonce: &StreamNonce) -> StreamDecryptor {
    let mut raw = Key::default();
    raw.copy_from_slice(key);
    let decryptor = DecryptorBE32::new(&raw, nonce);
    raw.as_mut_slice().zeroize();
    decryptor
}

fn stream_nonce(prefix: &[u8; STREAM_NONCE_PREFIX_LEN]) -> StreamNonce {
    let mut value = StreamNonce::default();
    value.copy_from_slice(prefix);
    value
}

fn encode_header(header: &FileHeader) -> [u8; HEADER_LEN] {
    let mut out = [0u8; HEADER_LEN];
    out[0..4].copy_from_slice(FILE_MAGIC);
    out[4..6].copy_from_slice(&FILE_VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&(HEADER_LEN as u16).to_le_bytes());
    out[8..24].copy_from_slice(&header.file_id);
    out[24..43].copy_from_slice(&header.nonce_prefix);
    out[43..51].copy_from_slice(&header.original_size.to_le_bytes());
    out[51..55].copy_from_slice(&header.chunk_size.to_le_bytes());
    out
}

fn parse_header(bytes: &[u8; HEADER_LEN]) -> Result<FileHeader, VaultError> {
    if &bytes[0..4] != FILE_MAGIC {
        return Err(VaultError::integrity());
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != FILE_VERSION {
        return Err(VaultError::integrity());
    }
    let header_len = u16::from_le_bytes([bytes[6], bytes[7]]);
    if header_len as usize != HEADER_LEN || bytes[55..].iter().any(|byte| *byte != 0) {
        return Err(VaultError::integrity());
    }

    let mut file_id = [0u8; FILE_ID_LEN];
    file_id.copy_from_slice(&bytes[8..24]);
    let mut nonce_prefix = [0u8; STREAM_NONCE_PREFIX_LEN];
    nonce_prefix.copy_from_slice(&bytes[24..43]);
    let original_size = u64::from_le_bytes(
        bytes[43..51]
            .try_into()
            .map_err(|_| VaultError::integrity())?,
    );
    let chunk_size = u32::from_le_bytes(
        bytes[51..55]
            .try_into()
            .map_err(|_| VaultError::integrity())?,
    );
    if !valid_chunk_layout(original_size, chunk_size) {
        return Err(VaultError::integrity());
    }

    Ok(FileHeader {
        file_id,
        nonce_prefix,
        original_size,
        chunk_size,
    })
}

fn file_aad(header: &[u8; HEADER_LEN]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(FILE_AAD_PREFIX.len() + HEADER_LEN);
    aad.extend_from_slice(FILE_AAD_PREFIX);
    aad.extend_from_slice(header);
    aad
}

/// FR-45：不使用密钥校验密文文件头与索引条目一致，并返回文件实际字节数。
///
/// 只读取未加密的 64 字节文件头与文件长度，不执行任何解密，也不修改文件；
/// 用于恢复前发现缺失、损坏或被替换的密文。认证与内容校验仍由解锁流程负责。
pub(crate) fn validate_ciphertext_structure(
    path: &Path,
    entry: &IndexEntry,
) -> Result<u64, VaultError> {
    let mut file = open_regular_file(path).map_err(|_| VaultError::integrity())?;
    let info = file.metadata().map_err(|_| VaultError::integrity())?;
    let mut header_bytes = [0u8; HEADER_LEN];
    file.read_exact(&mut header_bytes)
        .map_err(|_| VaultError::integrity())?;
    let header = parse_header(&header_bytes)?;
    let id_bytes = hex_decode_16(&entry.id).ok_or_else(VaultError::integrity)?;
    if header.file_id != id_bytes
        || header.original_size != entry.size
        || header.chunk_size != entry.chunk_size
        || !valid_chunk_layout(entry.size, entry.chunk_size)
    {
        return Err(VaultError::integrity());
    }
    // 每个分块的密文长度都包含独立认证标签，可与头部信息推算总长度。
    let count = chunk_count(entry.size, entry.chunk_size);
    let expected = HEADER_LEN as u64 + entry.size + count * TAG_LEN as u64;
    if info.len() != expected {
        return Err(VaultError::integrity());
    }
    Ok(info.len())
}

fn map_write_error(error: &io::Error) -> VaultError {
    if error.kind() == ErrorKind::StorageFull || error.raw_os_error() == Some(28) {
        VaultError::no_space()
    } else {
        VaultError::disk()
    }
}

fn map_read_error(error: &io::Error) -> VaultError {
    if error.kind() == ErrorKind::UnexpectedEof {
        VaultError::integrity()
    } else {
        VaultError::disk()
    }
}

fn open_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn create_dir_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn set_dir_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
}

/// 库级共享/排他锁。读密文持共享锁，索引与密文写操作持排他锁。
/// 所有锁均为非阻塞，进程退出时由内核自动释放。
pub(crate) struct VaultLock {
    file: File,
}

impl VaultLock {
    pub(crate) fn acquire(vault_dir: &Path) -> Result<Self, VaultError> {
        Self::acquire_with_mode(vault_dir, libc::LOCK_EX)
    }

    pub(crate) fn acquire_shared(vault_dir: &Path) -> Result<Self, VaultError> {
        Self::acquire_with_mode(vault_dir, libc::LOCK_SH)
    }

    fn acquire_with_mode(vault_dir: &Path, mode: i32) -> Result<Self, VaultError> {
        let path = vault_dir.join(".haven.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|_| VaultError::vault_unavailable())?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: `file` 在调用期间保持打开，flock 只修改该描述符的锁状态。
            let result = unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) };
            if result != 0 {
                return Err(VaultError::vault_busy());
            }
        }
        Ok(Self { file })
    }
}

impl Drop for VaultLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: 描述符仍有效；解锁失败不影响后续由进程退出释放的锁。
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn ensure_layout(vault_dir: &Path) -> Result<(), VaultError> {
    if !vault_dir.is_dir() {
        return Err(VaultError::vault_unavailable());
    }
    for name in [FILES_DIR, TMP_DIR] {
        let path = vault_dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(info) => {
                // 拒绝符号链接与普通文件，避免把清理/权限操作导向库外。
                if info.file_type().is_symlink() || !info.is_dir() {
                    return Err(VaultError::vault_unavailable());
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                create_dir_private(&path).map_err(|create| map_write_error(&create))?;
            }
            Err(_) => return Err(VaultError::vault_unavailable()),
        }
        set_dir_private(&path);
    }
    Ok(())
}

fn fsync_dir(path: &Path) -> io::Result<()> {
    File::open(path).and_then(|handle| handle.sync_all())
}

pub(crate) fn ciphertext_path(vault_dir: &Path, id: &str) -> PathBuf {
    vault_dir
        .join(FILES_DIR)
        .join(&id[..2])
        .join(format!("{id}.hvn"))
}

/// 打开普通文件：先拒绝符号链接、FIFO、设备等特殊文件，避免越界读取或阻塞。
fn open_regular_file(path: &Path) -> io::Result<File> {
    let info = fs::symlink_metadata(path)?;
    if info.file_type().is_symlink() || !info.is_file() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 即便检查后被替换成符号链接，也由内核拒绝打开。
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn allocate_file(vault_dir: &Path) -> Result<([u8; FILE_ID_LEN], String, PathBuf), VaultError> {
    for _ in 0..8 {
        let bytes = <[u8; FILE_ID_LEN]>::try_generate().map_err(|_| VaultError::internal())?;
        let id = hex_encode(&bytes);
        let path = ciphertext_path(vault_dir, &id);
        match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok((bytes, id, path)),
            Ok(_) => continue,
            Err(_) => return Err(VaultError::vault_unavailable()),
        }
    }
    Err(VaultError::file_exists())
}

fn temp_path(vault_dir: &Path) -> Result<PathBuf, VaultError> {
    Ok(vault_dir
        .join(TMP_DIR)
        .join(format!("import-{}.part", random_suffix()?)))
}

/// 流式加密：写入 64 字节文件头与 STREAM 分块密文，返回文件头与明文 SHA-256。
pub fn encrypt_stream<R: Read, W: Write>(
    reader: &mut R,
    size: u64,
    writer: &mut W,
    master_key: &[u8; 32],
    file_id: [u8; FILE_ID_LEN],
    cancel: &dyn Fn() -> bool,
) -> Result<([u8; HEADER_LEN], [u8; 32]), VaultError> {
    if !valid_chunk_layout(size, CHUNK_SIZE as u32) {
        return Err(VaultError::internal());
    }

    let nonce_prefix =
        <[u8; STREAM_NONCE_PREFIX_LEN]>::try_generate().map_err(|_| VaultError::internal())?;
    let header = FileHeader {
        file_id,
        nonce_prefix,
        original_size: size,
        chunk_size: CHUNK_SIZE as u32,
    };
    let header_bytes = encode_header(&header);
    writer
        .write_all(&header_bytes)
        .map_err(|err| map_write_error(&err))?;

    let aad = file_aad(&header_bytes);
    let key = file_key(master_key, &file_id);
    let mut encryptor = Some(make_encryptor(&key, &stream_nonce(&nonce_prefix)));
    let mut hasher = Sha256::new();
    let mut buffer = Zeroizing::new(vec![0u8; CHUNK_SIZE]);
    let mut remaining = size;

    loop {
        if cancel() {
            return Err(VaultError::import_cancelled());
        }
        let take = remaining.min(CHUNK_SIZE as u64) as usize;
        reader
            .read_exact(&mut buffer[..take])
            .map_err(|err| match err.kind() {
                ErrorKind::UnexpectedEof => VaultError::source_changed(),
                _ => VaultError::source_unreadable(),
            })?;
        hasher.update(&buffer[..take]);
        remaining -= take as u64;

        let mut block = Zeroizing::new(Vec::with_capacity(take + TAG_LEN));
        block.extend_from_slice(&buffer[..take]);

        if remaining == 0 {
            let stream = encryptor.take().ok_or_else(VaultError::internal)?;
            stream
                .encrypt_last_in_place(&aad, &mut *block)
                .map_err(|_| VaultError::integrity())?;
            writer
                .write_all(&block)
                .map_err(|err| map_write_error(&err))?;
            break;
        }

        encryptor
            .as_mut()
            .ok_or_else(VaultError::internal)?
            .encrypt_next_in_place(&aad, &mut *block)
            .map_err(|_| VaultError::integrity())?;
        writer
            .write_all(&block)
            .map_err(|err| map_write_error(&err))?;
    }

    let digest = hasher.finalize();
    let mut sha = [0u8; 32];
    sha.copy_from_slice(&digest);
    Ok((header_bytes, sha))
}

/// 流式解密并校验：文件头、索引绑定、每块认证、末尾多余字节与整体 SHA-256。
pub fn decrypt_stream<R: Read, S: FnMut(&[u8])>(
    reader: &mut R,
    entry: &IndexEntry,
    master_key: &[u8; 32],
    cancel: &dyn Fn() -> bool,
    sink: &mut S,
) -> Result<(), VaultError> {
    let mut header_bytes = [0u8; HEADER_LEN];
    reader
        .read_exact(&mut header_bytes)
        .map_err(|err| map_read_error(&err))?;
    let header = parse_header(&header_bytes)?;

    let id_bytes = hex_decode_16(&entry.id).ok_or_else(VaultError::integrity)?;
    if header.file_id != id_bytes
        || header.original_size != entry.size
        || header.chunk_size != entry.chunk_size
        || !valid_chunk_layout(entry.size, entry.chunk_size)
    {
        return Err(VaultError::integrity());
    }

    let expected: [u8; 32] = STANDARD
        .decode(entry.sha256.as_bytes())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(VaultError::integrity)?;

    let aad = file_aad(&header_bytes);
    let key = file_key(master_key, &header.file_id);
    let mut decryptor = Some(make_decryptor(&key, &stream_nonce(&header.nonce_prefix)));
    let mut hasher = Sha256::new();

    let count = chunk_count(header.original_size, header.chunk_size);
    let mut processed = 0u64;
    for index in 0..count {
        if cancel() {
            return Err(VaultError::import_cancelled());
        }
        let plain_len = (header.original_size - processed).min(header.chunk_size as u64) as usize;
        let cipher_len = plain_len + TAG_LEN;
        let mut block = Zeroizing::new(vec![0u8; cipher_len]);
        reader
            .read_exact(&mut block[..])
            .map_err(|err| map_read_error(&err))?;

        if index == count - 1 {
            let stream = decryptor.take().ok_or_else(VaultError::internal)?;
            stream
                .decrypt_last_in_place(&aad, &mut *block)
                .map_err(|_| VaultError::integrity())?;
        } else {
            decryptor
                .as_mut()
                .ok_or_else(VaultError::internal)?
                .decrypt_next_in_place(&aad, &mut *block)
                .map_err(|_| VaultError::integrity())?;
        }

        if block.len() != plain_len {
            return Err(VaultError::integrity());
        }
        hasher.update(&block);
        sink(&block);
        processed += plain_len as u64;
    }

    let mut extra = [0u8; 1];
    match reader.read(&mut extra) {
        Ok(0) => {}
        Ok(_) => return Err(VaultError::integrity()),
        Err(err) => return Err(map_read_error(&err)),
    }

    if processed != header.original_size || hasher.finalize().as_slice() != expected {
        return Err(VaultError::integrity());
    }
    Ok(())
}

fn verify_encrypted_file(
    path: &Path,
    entry: &IndexEntry,
    master_key: &[u8; 32],
) -> Result<(), VaultError> {
    let mut file = open_regular_file(path).map_err(|_| VaultError::integrity())?;
    let mut sink = |_: &[u8]| {};
    decrypt_stream(&mut file, entry, master_key, &|| false, &mut sink)
}

/// 图片预览的明文上限（20 MiB）。超过后拒绝解密，避免一次占用过多内存。
pub const PREVIEW_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// 图片解码后的总像素上限（1600 万），限制 WebView 解码时的内存占用。
pub const PREVIEW_MAX_PIXELS: u64 = 16_000_000;

/// 图片解码后的单边像素上限。
pub const PREVIEW_MAX_DIMENSION: u32 = 8192;

const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF];
const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const PDF_MAGIC: &[u8] = b"%PDF-";

fn preview_unsupported() -> VaultError {
    VaultError::new("PREVIEW_UNSUPPORTED", "该文件类型暂不支持预览。")
}

fn preview_too_large() -> VaultError {
    VaultError::new(
        "PREVIEW_TOO_LARGE",
        "图片超过 20 MB 的预览上限，暂不支持预览。",
    )
}

fn preview_invalid() -> VaultError {
    VaultError::new("PREVIEW_INVALID", "文件内容不是有效的 JPG/PNG 图片。")
}

fn preview_dimensions_too_large() -> VaultError {
    VaultError::new(
        "PREVIEW_TOO_LARGE",
        "图片解码尺寸超过预览上限（最大 1600 万像素、单边 8192 px）。",
    )
}

/// 解析 PNG 的 IHDR，返回 (宽, 高)。
///
/// 只读取签名之后的第一个块，且要求块长度恰为 13、类型为 `IHDR`。
/// 任何截断、非法块或异常长度都会返回 `None`，交由调用方按无效图片处理。
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || !bytes.starts_with(PNG_MAGIC) {
        return None;
    }
    let length = u32::from_be_bytes(bytes[8..12].try_into().ok()?);
    if length != 13 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// 判断标记是否为 SOF（Start of Frame），排除 DHT/JPG/DAC 等非帧头标记。
fn is_sof_marker(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC)
}

/// 逐段解析 JPEG 标记，返回首个 SOF 中的 (宽, 高)。
///
/// 不使用固定偏移：先跳过多余的 `0xFF` 填充字节，再按段长度跳过 APPn、DQT 等
/// 任意前置段。遇到 SOS/EOI、未知的段长度、截断或标记失步时返回 `None`。
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || !bytes.starts_with(&JPEG_MAGIC[..2]) {
        return None;
    }
    let mut pos = 2usize;
    while pos < bytes.len() {
        if bytes[pos] != 0xFF {
            return None;
        }
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        let marker = *bytes.get(pos)?;
        pos += 1;
        match marker {
            // 无长度的独立标记。
            0x01 | 0xD0..=0xD8 => continue,
            // 扫描开始或文件结束前仍未找到 SOF。
            0xDA | 0xD9 => return None,
            _ => {}
        }
        let length = u16::from_be_bytes([*bytes.get(pos)?, *bytes.get(pos + 1)?]) as usize;
        // 段长度包含自身 2 字节，至少为 2；且必须完整落在缓冲区内。
        if length < 2 || pos.checked_add(length)? > bytes.len() {
            return None;
        }
        if is_sof_marker(marker) {
            // 精度 1 字节 + 高 2 字节 + 宽 2 字节。
            if length < 7 {
                return None;
            }
            let height = u16::from_be_bytes([bytes[pos + 3], bytes[pos + 4]]);
            let width = u16::from_be_bytes([bytes[pos + 5], bytes[pos + 6]]);
            return Some((width as u32, height as u32));
        }
        pos += length;
    }
    None
}

/// 在交给 WebView 解码之前校验图片尺寸，避免解码后占用过多内存。
fn check_preview_dimensions(magic: &[u8], bytes: &[u8]) -> Result<(), VaultError> {
    let dimensions = if magic == PNG_MAGIC {
        png_dimensions(bytes)
    } else {
        jpeg_dimensions(bytes)
    };
    let Some((width, height)) = dimensions else {
        return Err(preview_invalid());
    };
    if width == 0 || height == 0 {
        return Err(preview_invalid());
    }
    // 用 u64 计算，避免宽高相乘或边界判断发生整数溢出。
    if width > PREVIEW_MAX_DIMENSION
        || height > PREVIEW_MAX_DIMENSION
        || u64::from(width) * u64::from(height) > PREVIEW_MAX_PIXELS
    {
        return Err(preview_dimensions_too_large());
    }
    Ok(())
}

fn file_missing() -> VaultError {
    VaultError::new("FILE_MISSING", "密文文件不存在。")
}

/// 依据名称判断预览类型，返回期望的文件头。
fn preview_kind(name: &str) -> Option<&'static [u8]> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some(JPEG_MAGIC)
    } else if lower.ends_with(".png") {
        Some(PNG_MAGIC)
    } else {
        None
    }
}

/// 在内存中解密一张图片用于预览，返回经认证的明文。
///
/// - 只接受索引中登记过的条目，并由调用方保证条目来自加密索引。
/// - 复用 `decrypt_stream` 的全部校验（头绑定、逐块认证、截断、尾部、SHA-256）。
/// - 明文只存在内存中，不写入任何临时文件。
pub fn preview_image(
    vault_dir: &Path,
    entry: &IndexEntry,
    master_key: &[u8; 32],
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>, VaultError> {
    let Some(magic) = preview_kind(&entry.name) else {
        return Err(preview_unsupported());
    };
    if entry.size > PREVIEW_MAX_BYTES {
        return Err(preview_too_large());
    }

    let path = ciphertext_path(vault_dir, &entry.id);
    let mut file = match open_regular_file(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Err(file_missing()),
        Err(_) => return Err(VaultError::vault_unavailable()),
    };

    let mut plain = Zeroizing::new(Vec::with_capacity(entry.size as usize));
    let mut sink = |chunk: &[u8]| plain.extend_from_slice(chunk);
    decrypt_stream(&mut file, entry, master_key, cancel, &mut sink)?;

    if plain.len() as u64 != entry.size || !plain.starts_with(magic) {
        return Err(preview_invalid());
    }
    // 在把明文交给前端创建 Blob、进入 WebView 解码之前，先限制解码尺寸。
    check_preview_dimensions(magic, &plain)?;
    // 把明文移出 Zeroizing 容器交给 IPC；原缓冲已为空，不再保留第二份副本。
    Ok(std::mem::take(&mut *plain))
}

/// 视频预览的明文上限（64 MiB）。
///
/// Tauri 的二进制 IPC 会把认证后的字节复制到 WebView，再创建 Blob；
/// 64 MiB 在桌面上仍有约数倍瞬时内存占用，因此不追求覆盖超大视频，
/// 超过上限时直接给出明确提示，避免牺牲稳定性与安全性。
pub const VIDEO_PREVIEW_MAX_BYTES: u64 = 64 * 1024 * 1024;

const WEBM_MAGIC: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3];

fn video_too_large() -> VaultError {
    VaultError::new(
        "PREVIEW_TOO_LARGE",
        "视频超过 64 MB 的预览上限，暂不支持在线预览。",
    )
}

fn video_invalid() -> VaultError {
    VaultError::new("PREVIEW_INVALID", "文件内容不是有效的视频。")
}

/// 返回视频容器类型：`iso` 表示 MP4/MOV/M4V，`webm` 表示 WebM。
fn video_container(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".mp4") || lower.ends_with(".mov") || lower.ends_with(".m4v") {
        Some("iso")
    } else if lower.ends_with(".webm") {
        Some("webm")
    } else {
        None
    }
}

/// 校验视频容器头：ISO BMFF 的前 8 字节为长度 + 已知顶层原子，
/// WebM 以 EBML 头开始。兼容以 `moov`/`mdat`/`wide` 开头的旧 MOV。
fn has_video_header(container: &str, bytes: &[u8]) -> bool {
    if container == "webm" {
        return bytes.starts_with(WEBM_MAGIC);
    }
    const ISO_ATOMS: [&[u8; 4]; 6] = [b"ftyp", b"moov", b"mdat", b"wide", b"free", b"skip"];
    bytes.len() >= 8 && ISO_ATOMS.iter().any(|atom| &bytes[4..8] == *atom)
}

/// 在内存中解密一个视频文件，返回经认证的明文供前端创建 Blob。
///
/// - 复用 `decrypt_stream` 的逐块认证、截断检测与 SHA-256 校验。
/// - 先校验扩展名、大小与容器头，再交给 WebView 原生 `<video>` 解码。
/// - 明文不写入任何临时文件，密钥不离开 Rust 端。
pub fn preview_video(
    vault_dir: &Path,
    entry: &IndexEntry,
    master_key: &[u8; 32],
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>, VaultError> {
    let Some(container) = video_container(&entry.name) else {
        return Err(preview_unsupported());
    };
    if entry.size > VIDEO_PREVIEW_MAX_BYTES {
        return Err(video_too_large());
    }

    let path = ciphertext_path(vault_dir, &entry.id);
    let mut file = match open_regular_file(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Err(file_missing()),
        Err(_) => return Err(VaultError::vault_unavailable()),
    };

    let mut plain = Zeroizing::new(Vec::with_capacity(entry.size as usize));
    let mut sink = |chunk: &[u8]| plain.extend_from_slice(chunk);
    decrypt_stream(&mut file, entry, master_key, cancel, &mut sink)?;

    if plain.len() as u64 != entry.size || !has_video_header(container, &plain) {
        return Err(video_invalid());
    }
    Ok(std::mem::take(&mut *plain))
}

/// 网格缩略图的最大边长（px）。
pub const THUMBNAIL_MAX_DIMENSION: u32 = 320;

/// 在内存中解密图片并生成 PNG 缩略图，供网格视图展示。
///
/// - 复用 `preview_image` 的 20 MiB 上限、格式识别与像素上限。
/// - 解码与缩放全部在 Rust 端完成，前端只拿到缩略图字节，不接触原图明文。
/// - 明文在解码后立即清零，缩略图不写入磁盘或任何缓存目录。
pub fn preview_thumbnail(
    vault_dir: &Path,
    entry: &IndexEntry,
    master_key: &[u8; 32],
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>, VaultError> {
    let Some(magic) = preview_kind(&entry.name) else {
        return Err(preview_unsupported());
    };
    if entry.size > PREVIEW_MAX_BYTES {
        return Err(preview_too_large());
    }

    let path = ciphertext_path(vault_dir, &entry.id);
    let mut file = match open_regular_file(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Err(file_missing()),
        Err(_) => return Err(VaultError::vault_unavailable()),
    };

    let mut plain = Zeroizing::new(Vec::with_capacity(entry.size as usize));
    let mut sink = |chunk: &[u8]| plain.extend_from_slice(chunk);
    decrypt_stream(&mut file, entry, master_key, cancel, &mut sink)?;

    if plain.len() as u64 != entry.size || !plain.starts_with(magic) {
        return Err(preview_invalid());
    }
    check_preview_dimensions(magic, &plain)?;

    let format = if magic == PNG_MAGIC {
        image::ImageFormat::Png
    } else {
        image::ImageFormat::Jpeg
    };
    let decoded =
        image::load_from_memory_with_format(&plain, format).map_err(|_| preview_invalid())?;
    // 解码结束后立即清零原图明文，只保留缩放后的像素。
    drop(plain);

    let thumbnail = decoded.thumbnail(THUMBNAIL_MAX_DIMENSION, THUMBNAIL_MAX_DIMENSION);
    let mut out = Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|_| preview_invalid())?;
    Ok(out.into_inner())
}

fn preview_pdf_too_large() -> VaultError {
    VaultError::new(
        "PREVIEW_TOO_LARGE",
        "PDF 超过 20 MB 的预览上限，暂不支持预览。",
    )
}

fn preview_pdf_invalid() -> VaultError {
    VaultError::new("PREVIEW_INVALID", "文件内容不是有效的 PDF。")
}

/// PDF 规范允许文件头出现在前 1024 字节内，这里按同一规则校验。
fn has_pdf_header(bytes: &[u8]) -> bool {
    let window = bytes.len().min(1024);
    bytes[..window]
        .windows(PDF_MAGIC.len())
        .any(|chunk| chunk == PDF_MAGIC)
}

/// 在内存中解密一个 PDF 用于预览，返回经认证的明文。
///
/// - 复用 `preview_image` 的大小上限与 `decrypt_stream` 的全部校验。
/// - 明文不写入任何临时文件，前端只拿到字节，用于本地画布渲染。
pub fn preview_pdf(
    vault_dir: &Path,
    entry: &IndexEntry,
    master_key: &[u8; 32],
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>, VaultError> {
    if !entry.name.to_ascii_lowercase().ends_with(".pdf") {
        return Err(preview_unsupported());
    }
    if entry.size > PREVIEW_MAX_BYTES {
        return Err(preview_pdf_too_large());
    }

    let path = ciphertext_path(vault_dir, &entry.id);
    let mut file = match open_regular_file(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Err(file_missing()),
        Err(_) => return Err(VaultError::vault_unavailable()),
    };

    let mut plain = Zeroizing::new(Vec::with_capacity(entry.size as usize));
    let mut sink = |chunk: &[u8]| plain.extend_from_slice(chunk);
    decrypt_stream(&mut file, entry, master_key, cancel, &mut sink)?;

    if plain.len() as u64 != entry.size || !has_pdf_header(&plain) {
        return Err(preview_pdf_invalid());
    }
    Ok(std::mem::take(&mut *plain))
}

fn has_unsafe_display_chars(name: &str) -> bool {
    name.chars().any(|ch| {
        ch.is_control()
            // 双向控制符与方向标记可伪造扩展名显示，统一视为不可展示。
            || matches!(
                ch,
                '\u{061C}'
                    | '\u{200E}'
                    | '\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2066}'..='\u{2069}'
            )
    })
}

fn sanitize_name(path: &Path) -> String {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name)
            if !name.is_empty()
                && !has_unsafe_display_chars(name)
                && name.chars().count() <= 255 =>
        {
            name.to_string()
        }
        _ => "未命名文件".to_string(),
    }
}

fn dedup_name(base: &str, used: &HashSet<String>) -> (String, bool) {
    if !used.contains(base) {
        return (base.to_string(), false);
    }
    let path = Path::new(base);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(base);
    let extension = path.extension().and_then(|value| value.to_str());
    let mut ordinal = 1u32;
    loop {
        let candidate = match extension {
            Some(extension) => format!("{stem} ({ordinal}).{extension}"),
            None => format!("{stem} ({ordinal})"),
        };
        if !used.contains(&candidate) {
            return (candidate, true);
        }
        if ordinal == u32::MAX {
            return (base.to_string(), true);
        }
        ordinal += 1;
    }
}

impl Index {
    /// 读取并解密索引。索引缺失且库内没有任何密文时视为空索引。
    pub fn load(vault_dir: &Path, master_key: &[u8; 32]) -> Result<Self, VaultError> {
        let path = vault_dir.join(INDEX_FILE);
        let mut file = match open_regular_file(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                // 删除暂存同样代表库内可能存在数据，不能在索引缺失时重建空索引。
                if scan_ciphertext_ids(vault_dir)?.is_empty()
                    && scan_delete_staging(vault_dir)?.is_empty()
                {
                    return Ok(Self::default());
                }
                return Err(VaultError::index_missing());
            }
            Err(_) => return Err(VaultError::index_invalid()),
        };

        let mut header = [0u8; INDEX_HEADER_LEN];
        file.read_exact(&mut header)
            .map_err(|_| VaultError::index_invalid())?;
        if &header[0..4] != INDEX_MAGIC {
            return Err(VaultError::index_invalid());
        }
        let version = u32::from_le_bytes(
            header[4..8]
                .try_into()
                .map_err(|_| VaultError::index_invalid())?,
        );
        if version != INDEX_VERSION {
            return Err(VaultError::index_invalid());
        }

        let mut salt = [0u8; INDEX_SALT_LEN];
        salt.copy_from_slice(&header[8..24]);
        let mut nonce_bytes = [0u8; INDEX_NONCE_LEN];
        nonce_bytes.copy_from_slice(&header[24..48]);
        let plain_len = u64::from_le_bytes(
            header[48..56]
                .try_into()
                .map_err(|_| VaultError::index_invalid())?,
        );
        if plain_len > MAX_INDEX_BYTES {
            return Err(VaultError::index_invalid());
        }
        // 未认证的长度字段只有在与实际文件长度一致时才可信，避免按伪造长度分配内存。
        let expected_len = INDEX_HEADER_LEN as u64 + plain_len + TAG_LEN as u64;
        match file.metadata() {
            Ok(meta) if meta.len() == expected_len => {}
            _ => return Err(VaultError::index_invalid()),
        }

        let mut ciphertext = vec![0u8; plain_len as usize + TAG_LEN];
        file.read_exact(&mut ciphertext)
            .map_err(|_| VaultError::index_invalid())?;
        let mut extra = [0u8; 1];
        match file.read(&mut extra) {
            Ok(0) => {}
            Ok(_) => return Err(VaultError::index_invalid()),
            Err(_) => return Err(VaultError::index_invalid()),
        }

        let key = derive_key(master_key, &salt, INDEX_KEY_INFO);
        let cipher = make_cipher(&key);
        let nonce = XNonce::try_from(&nonce_bytes[..]).map_err(|_| VaultError::index_invalid())?;
        let mut aad = Vec::with_capacity(INDEX_AAD_PREFIX.len() + INDEX_HEADER_LEN);
        aad.extend_from_slice(INDEX_AAD_PREFIX);
        aad.extend_from_slice(&header);

        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    &nonce,
                    Payload {
                        msg: &ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| VaultError::index_invalid())?,
        );
        let parsed: IndexFile =
            serde_json::from_slice(&plaintext).map_err(|_| VaultError::index_invalid())?;
        if parsed.format != "haven-index" || parsed.version != INDEX_VERSION {
            return Err(VaultError::index_invalid());
        }
        let mut ids = HashSet::new();
        for entry in &parsed.entries {
            if !valid_entry(entry) || !ids.insert(entry.id.clone()) {
                return Err(VaultError::index_invalid());
            }
        }
        let mut folder_ids = HashSet::new();
        for folder in &parsed.folders {
            if !valid_folder(folder) || !folder_ids.insert(folder.id.clone()) {
                return Err(VaultError::index_invalid());
            }
        }

        Ok(Self {
            entries: parsed.entries,
            folders: parsed.folders,
        })
    }

    /// 以全新随机 salt 与 nonce 加密并原子替换索引文件。
    ///
    /// rename 之前的错误是 `NotCommitted`；rename 成功但目录同步失败是 `Unknown`。
    /// 调用方在 `Unknown` 时不得按旧索引仍生效进行回滚或删除密文。
    fn save(&self, vault_dir: &Path, master_key: &[u8; 32]) -> Result<IndexCommit, IndexSaveError> {
        let not_committed = |error| Err(IndexSaveError::not_committed(error));
        if !vault_dir.is_dir() {
            return not_committed(VaultError::vault_unavailable());
        }
        let payload = Zeroizing::new(
            serde_json::to_vec(&IndexFile {
                format: "haven-index".to_string(),
                version: INDEX_VERSION,
                entries: self.entries.clone(),
                folders: self.folders.clone(),
            })
            .map_err(|_| VaultError::internal())
            .map_err(IndexSaveError::not_committed)?,
        );
        if payload.len() as u64 > MAX_INDEX_BYTES {
            return not_committed(VaultError::index_invalid());
        }

        let salt = <[u8; INDEX_SALT_LEN]>::try_generate()
            .map_err(|_| VaultError::internal())
            .map_err(IndexSaveError::not_committed)?;
        let nonce_bytes = <[u8; INDEX_NONCE_LEN]>::try_generate()
            .map_err(|_| VaultError::internal())
            .map_err(IndexSaveError::not_committed)?;
        let mut header = [0u8; INDEX_HEADER_LEN];
        header[0..4].copy_from_slice(INDEX_MAGIC);
        header[4..8].copy_from_slice(&INDEX_VERSION.to_le_bytes());
        header[8..24].copy_from_slice(&salt);
        header[24..48].copy_from_slice(&nonce_bytes);
        header[48..56].copy_from_slice(&(payload.len() as u64).to_le_bytes());

        let key = derive_key(master_key, &salt, INDEX_KEY_INFO);
        let cipher = make_cipher(&key);
        let nonce = XNonce::try_from(&nonce_bytes[..])
            .map_err(|_| VaultError::internal())
            .map_err(IndexSaveError::not_committed)?;
        let mut aad = Vec::with_capacity(INDEX_AAD_PREFIX.len() + INDEX_HEADER_LEN);
        aad.extend_from_slice(INDEX_AAD_PREFIX);
        aad.extend_from_slice(&header);

        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: payload.as_slice(),
                    aad: &aad,
                },
            )
            .map_err(|_| VaultError::internal())
            .map_err(IndexSaveError::not_committed)?;

        let suffix = random_suffix().map_err(IndexSaveError::not_committed)?;
        let temp_path = vault_dir.join(format!(".index.tmp-{suffix}"));
        let write_result = (|| -> io::Result<()> {
            let mut file = open_private_file(&temp_path)?;
            file.write_all(&header)?;
            file.write_all(&ciphertext)?;
            file.sync_all()
        })();
        if let Err(err) = write_result {
            let _ = fs::remove_file(&temp_path);
            return not_committed(map_write_error(&err));
        }
        if let Err(err) = fs::rename(&temp_path, vault_dir.join(INDEX_FILE)) {
            let _ = fs::remove_file(&temp_path);
            return not_committed(map_write_error(&err));
        }
        // 测试注入：rename 已成功、目录同步失败，必须归类为 Unknown。
        #[cfg(test)]
        if fault_injection::index_save_fsync_failure() {
            return Err(IndexSaveError::unknown(VaultError::disk()));
        }
        fsync_dir(vault_dir).map_err(|err| IndexSaveError::unknown(map_write_error(&err)))?;
        Ok(IndexCommit::Committed)
    }
}

fn valid_entry(entry: &IndexEntry) -> bool {
    hex_decode_16(&entry.id).is_some()
        && !entry.name.is_empty()
        && !has_unsafe_display_chars(&entry.name)
        && entry.name.chars().count() <= 1024
        && valid_chunk_layout(entry.size, entry.chunk_size)
        && STANDARD
            .decode(entry.sha256.as_bytes())
            .map(|bytes| bytes.len() == 32)
            .unwrap_or(false)
}

pub(crate) fn valid_file_id(id: &str) -> bool {
    id.len() == FILE_ID_LEN * 2
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && hex_decode_16(id).is_some()
}

/// 索引恢复时的最小文件夹校验：稳定 ID、非空安全名称与长度上限。
fn valid_folder(folder: &FolderEntry) -> bool {
    valid_file_id(&folder.id)
        && !folder.name.is_empty()
        && !has_unsafe_display_chars(&folder.name)
        && folder.name.chars().count() <= FOLDER_NAME_MAX_CHARS
}

fn delete_staging_path(vault_dir: &Path, id: &str) -> PathBuf {
    vault_dir.join(TMP_DIR).join(format!("delete-{id}.hvn"))
}

fn parse_delete_staging_name(name: &str) -> Option<String> {
    let id = name.strip_prefix("delete-")?.strip_suffix(".hvn")?;
    valid_file_id(id).then(|| id.to_string())
}

fn parse_delete_staging_os_name(name: &OsStr) -> Result<Option<String>, VaultError> {
    if !name.as_encoded_bytes().starts_with(b"delete-") {
        return Ok(None);
    }
    let name = name.to_str().ok_or_else(VaultError::recovery_required)?;
    parse_delete_staging_name(name)
        .map(Some)
        .ok_or_else(VaultError::recovery_required)
}

#[derive(Debug)]
struct DeleteStaging {
    id: String,
    path: PathBuf,
}

/// 只识别删除事务自己的暂存命名；任何可疑的 delete-* 条目都阻止自动恢复。
fn scan_delete_staging(vault_dir: &Path) -> Result<Vec<DeleteStaging>, VaultError> {
    let tmp_dir = vault_dir.join(TMP_DIR);
    let info = match fs::symlink_metadata(&tmp_dir) {
        Ok(info) => info,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(VaultError::vault_unavailable()),
    };
    if info.file_type().is_symlink() || !info.is_dir() {
        return Err(VaultError::vault_unavailable());
    }
    let entries = fs::read_dir(tmp_dir).map_err(|_| VaultError::vault_unavailable())?;
    let mut staged = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| VaultError::vault_unavailable())?;
        let name = entry.file_name();
        let Some(id) = parse_delete_staging_os_name(&name)? else {
            continue;
        };
        staged.push(DeleteStaging {
            id,
            path: entry.path(),
        });
    }
    Ok(staged)
}

/// 扫描 `files/` 下符合 Haven 命名规则的密文 ID。不跟随符号链接。
///
/// 目录不可读时必须返回错误而不是空集合：调用方用空集合判断“库内没有密文”，
/// 把 I/O 失败当成空库会导致在索引缺失时误建空索引，让既有密文变得不可达。
fn scan_ciphertext_ids(vault_dir: &Path) -> Result<Vec<String>, VaultError> {
    let mut ids = Vec::new();
    let files_dir = vault_dir.join(FILES_DIR);
    let info = fs::symlink_metadata(&files_dir).map_err(|_| VaultError::vault_unavailable())?;
    if info.file_type().is_symlink() || !info.is_dir() {
        return Err(VaultError::vault_unavailable());
    }
    let prefixes = fs::read_dir(&files_dir).map_err(|_| VaultError::vault_unavailable())?;
    for prefix in prefixes {
        let prefix = prefix.map_err(|_| VaultError::vault_unavailable())?;
        let kind = prefix
            .file_type()
            .map_err(|_| VaultError::vault_unavailable())?;
        // file_type 不跟随符号链接，符号链接目录会被跳过。
        if !kind.is_dir() {
            continue;
        }
        let entries = fs::read_dir(prefix.path()).map_err(|_| VaultError::vault_unavailable())?;
        for entry in entries {
            let entry = entry.map_err(|_| VaultError::vault_unavailable())?;
            let kind = entry
                .file_type()
                .map_err(|_| VaultError::vault_unavailable())?;
            if !kind.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(id) = name.strip_suffix(".hvn")
                && hex_decode_16(id).is_some()
            {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

fn cleanup_matching(dir: &Path, matches: impl Fn(&str) -> bool) {
    let Ok(info) = fs::symlink_metadata(dir) else {
        return;
    };
    if info.file_type().is_symlink() || !info.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !matches(&name) {
            continue;
        }
        if entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// 只清理本应用命名的临时文件，不触碰用户放入目录的任何其他文件。
fn cleanup_app_temp(vault_dir: &Path) {
    cleanup_matching(&vault_dir.join(TMP_DIR), |name| {
        name.starts_with("import-") && name.ends_with(".part")
    });
    cleanup_matching(vault_dir, |name| name.starts_with(".index.tmp-"));
}

fn require_real_dir(path: &Path) -> Result<(), VaultError> {
    let info = fs::symlink_metadata(path).map_err(|_| VaultError::vault_unavailable())?;
    if info.file_type().is_symlink() || !info.is_dir() {
        return Err(VaultError::vault_unavailable());
    }
    Ok(())
}

/// 恢复前先检查全部删除现场，避免处理到一半才发现另一份可疑文件。
fn recover_delete_files(vault_dir: &Path, index: &Index) -> Result<(), VaultError> {
    let staged = scan_delete_staging(vault_dir)?;
    for item in &staged {
        let staged_file =
            open_regular_file(&item.path).map_err(|_| VaultError::recovery_required())?;
        drop(staged_file);
        let original = ciphertext_path(vault_dir, &item.id);
        match fs::symlink_metadata(&original) {
            Ok(_) => return Err(VaultError::recovery_required()),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(VaultError::recovery_required()),
        }
        if index.entries.iter().any(|entry| entry.id == item.id) {
            let parent = original
                .parent()
                .ok_or_else(VaultError::recovery_required)?;
            require_real_dir(parent).map_err(|_| VaultError::recovery_required())?;
        }
    }

    for item in staged {
        let original = ciphertext_path(vault_dir, &item.id);
        if index.entries.iter().any(|entry| entry.id == item.id) {
            // S1：索引仍引用该 ID，删除未提交，恢复到原路径。
            commit_exclusive(&item.path, &original).map_err(|_| VaultError::recovery_required())?;
            let parent = original
                .parent()
                .ok_or_else(VaultError::recovery_required)?;
            fsync_dir(parent).map_err(|_| VaultError::recovery_required())?;
        } else {
            // S2：索引已提交删除，只清理严格校验过的暂存密文。
            fs::remove_file(&item.path).map_err(|_| VaultError::recovery_required())?;
        }
        fsync_dir(&vault_dir.join(TMP_DIR)).map_err(|_| VaultError::recovery_required())?;
    }
    Ok(())
}

/// 解锁并认证索引后恢复上次异常退出留下的删除事务，再清理普通临时数据。
///
/// 不删除索引中不存在的普通密文：多实例或备份回放场景下无法区分“未完成的导入”
/// 与“索引被替换”。只有严格命名的删除暂存文件才按上述状态机处理。
pub fn prepare_vault(vault_dir: &Path, master_key: &[u8; 32]) -> Result<(), VaultError> {
    let _lock = VaultLock::acquire(vault_dir)?;
    ensure_layout(vault_dir)?;
    let index_missing = matches!(
        fs::symlink_metadata(vault_dir.join(INDEX_FILE)),
        Err(err) if err.kind() == ErrorKind::NotFound
    );
    // 索引无法认证时直接返回，不清理或改写任何恢复现场。
    let index = Index::load(vault_dir, master_key)?;
    recover_delete_files(vault_dir, &index)?;
    cleanup_app_temp(vault_dir);

    if index_missing {
        match Index::default().save(vault_dir, master_key) {
            Ok(IndexCommit::Committed) => {}
            Ok(_) => return Err(VaultError::recovery_required()),
            Err(err) if err.commit == IndexCommit::NotCommitted => return Err(err.error),
            Err(_) => return Err(VaultError::recovery_required()),
        }
    }
    Ok(())
}

/// 把临时密文原子提交为最终文件，目标已存在时失败且绝不覆盖。
///
/// macOS 使用 `renamex_np(RENAME_EXCL)`：由内核在 rename 时检查目标是否存在，
/// 不存在“先检查再 rename”的竞态窗口。其他平台没有等价的 std API，退回
/// 存在性检查 + rename，此时依赖随机 file_id 的不可猜测性降低（而非消除）冲突概率。
fn commit_exclusive(temp: &Path, final_path: &Path) -> Result<(), VaultError> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(temp.as_os_str().as_bytes()).map_err(|_| VaultError::internal())?;
        let to =
            CString::new(final_path.as_os_str().as_bytes()).map_err(|_| VaultError::internal())?;
        // SAFETY: 两个指针均指向调用期间有效的 NUL 结尾字符串；renamex_np 不保留引用。
        let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        if result == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        Err(if err.kind() == ErrorKind::AlreadyExists {
            VaultError::file_exists()
        } else {
            map_write_error(&err)
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        match fs::symlink_metadata(final_path) {
            Ok(_) => return Err(VaultError::file_exists()),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(VaultError::vault_unavailable()),
        }
        fs::rename(temp, final_path).map_err(|err| map_write_error(&err))
    }
}

fn import_one(
    vault_dir: &Path,
    master_key: &[u8; 32],
    source: &Path,
    used_names: &HashSet<String>,
    cancel: &dyn Fn() -> bool,
) -> Result<(IndexEntry, PathBuf, bool), VaultError> {
    let metadata = fs::symlink_metadata(source).map_err(|err| {
        if err.kind() == ErrorKind::NotFound {
            VaultError::source_missing()
        } else {
            VaultError::source_unreadable()
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(VaultError::source_unreadable());
    }
    let size = metadata.len();
    let modified = metadata.modified().ok();
    let mut source_file = File::open(source).map_err(|_| VaultError::source_unreadable())?;
    // 打开后核对 inode，缩小检查与打开之间被替换为符号链接的窗口。
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = source_file
            .metadata()
            .map_err(|_| VaultError::source_unreadable())?;
        if !opened.is_file() || opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(VaultError::source_changed());
        }
    }

    let raw_name = sanitize_name(source);
    let (name, renamed) = dedup_name(&raw_name, used_names);

    let (id_bytes, id, final_path) = allocate_file(vault_dir)?;
    if let Some(parent) = final_path.parent() {
        match fs::symlink_metadata(parent) {
            Ok(info) => {
                // 前缀目录必须是我们创建的真实目录，拒绝被替换成符号链接。
                if info.file_type().is_symlink() || !info.is_dir() {
                    return Err(VaultError::vault_unavailable());
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                create_dir_private(parent).map_err(|err| map_write_error(&err))?;
            }
            Err(_) => return Err(VaultError::vault_unavailable()),
        }
        set_dir_private(parent);
    }
    let temp_path = temp_path(vault_dir)?;

    let prepared = (|| -> Result<IndexEntry, VaultError> {
        let mut temp = open_private_file(&temp_path).map_err(|err| map_write_error(&err))?;
        let (_, sha) = encrypt_stream(
            &mut source_file,
            size,
            &mut temp,
            master_key,
            id_bytes,
            cancel,
        )?;
        temp.sync_all().map_err(|err| map_write_error(&err))?;
        drop(temp);

        let after = fs::metadata(source).map_err(|_| VaultError::source_changed())?;
        if after.len() != size || (modified.is_some() && after.modified().ok() != modified) {
            return Err(VaultError::source_changed());
        }

        let entry = IndexEntry {
            id: id.clone(),
            name: name.clone(),
            size,
            chunk_size: CHUNK_SIZE as u32,
            imported_at: unix_now()?,
            sha256: STANDARD.encode(sha),
            favorite: false,
            folder_id: None,
        };
        verify_encrypted_file(&temp_path, &entry, master_key)?;
        Ok(entry)
    })();

    let entry = match prepared {
        Ok(entry) => entry,
        Err(err) => {
            let _ = fs::remove_file(&temp_path);
            return Err(err);
        }
    };

    if let Err(err) = commit_exclusive(&temp_path, &final_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }
    if let Some(parent) = final_path.parent() {
        let _ = fsync_dir(parent);
    }
    Ok((entry, final_path, renamed))
}

fn apply_import_index_outcome(
    outcome: Result<IndexCommit, IndexSaveError>,
    index: &mut Index,
    final_path: &Path,
) -> Result<(), VaultError> {
    match outcome {
        Ok(IndexCommit::Committed) => Ok(()),
        Err(err) if err.commit == IndexCommit::NotCommitted => {
            index.entries.pop();
            if fs::remove_file(final_path).is_ok()
                && let Some(parent) = final_path.parent()
            {
                let _ = fsync_dir(parent);
            }
            Err(err.error)
        }
        // rename 后同步失败时，新旧索引都可能生效，必须保留密文并受控恢复。
        _ => Err(VaultError::recovery_required()),
    }
}

fn scan_failure(path: &Path, error: VaultError) -> FailedFile {
    FailedFile {
        name: sanitize_name(path),
        code: error.code,
        message: error.message,
    }
}

/// 把导入路径展开为普通文件列表：目录递归收集，符号链接与特殊文件拒绝。
///
/// 扫描使用显式栈而不是递归，避免深层目录导致栈溢出；不跟随任何符号链接，
/// 因此不会出现路径循环。单个目录或条目不可读只记录一条失败，不影响其余条目。
fn expand_import_paths(
    paths: &[String],
    cancel: &dyn Fn() -> bool,
) -> (Vec<PathBuf>, Vec<FailedFile>, bool) {
    let mut files = Vec::new();
    let mut failures = Vec::new();

    for raw_path in paths {
        if cancel() {
            return (files, failures, true);
        }
        let path = Path::new(raw_path);
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(err) => {
                let error = if err.kind() == ErrorKind::NotFound {
                    VaultError::source_missing()
                } else {
                    VaultError::source_unreadable()
                };
                failures.push(scan_failure(path, error));
                continue;
            }
        };
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            failures.push(scan_failure(path, VaultError::source_unsupported()));
        } else if metadata.is_file() {
            files.push(path.to_path_buf());
        } else if walk_directory(path, &mut files, &mut failures, cancel) {
            return (files, failures, true);
        }
    }

    (files, failures, false)
}

/// 迭代遍历目录，返回是否被取消。目录内的普通文件按名称顺序收集。
fn walk_directory(
    root: &Path,
    files: &mut Vec<PathBuf>,
    failures: &mut Vec<FailedFile>,
    cancel: &dyn Fn() -> bool,
) -> bool {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if cancel() {
            return true;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => {
                failures.push(scan_failure(&dir, VaultError::source_unreadable()));
                continue;
            }
        };

        let mut names = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    failures.push(scan_failure(&dir, VaultError::source_unreadable()));
                    continue;
                }
            };
            names.push(entry.path());
        }
        names.sort();

        // 子目录逆序压栈，保证按名称升序展开。
        let mut subdirs = Vec::new();
        for child in names {
            let kind = match fs::symlink_metadata(&child) {
                Ok(metadata) => metadata,
                Err(_) => {
                    failures.push(scan_failure(&child, VaultError::source_unreadable()));
                    continue;
                }
            };
            if kind.file_type().is_symlink() {
                failures.push(scan_failure(&child, VaultError::source_unsupported()));
            } else if kind.is_file() {
                files.push(child);
            } else if kind.is_dir() {
                subdirs.push(child);
            } else {
                failures.push(scan_failure(&child, VaultError::source_unsupported()));
            }
        }
        for subdir in subdirs.into_iter().rev() {
            pending.push(subdir);
        }
    }
    false
}

/// 串行导入一批文件。单个失败不会中断其余文件；取消会停止后续文件。
pub fn import_batch(
    vault_dir: &Path,
    master_key: &[u8; 32],
    paths: &[String],
    cancel: &dyn Fn() -> bool,
    progress: &dyn Fn(ProgressEvent),
) -> Result<ImportSummary, VaultError> {
    let _vault_lock = VaultLock::acquire(vault_dir)?;
    ensure_layout(vault_dir)?;
    let mut index = Index::load(vault_dir, master_key)?;
    recover_delete_files(vault_dir, &index)?;
    cleanup_app_temp(vault_dir);
    let mut used_names: HashSet<String> = index
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect();

    // FR-36：先展开目录（含子目录），再按普通文件逐一导入。
    let (sources, scan_failures, scan_cancelled) = expand_import_paths(paths, cancel);
    let mut summary = ImportSummary {
        succeeded: Vec::new(),
        failed: scan_failures,
        cancelled: scan_cancelled,
    };
    if summary.cancelled {
        return Ok(summary);
    }
    let total = sources.len();

    for (position, source) in sources.iter().enumerate() {
        if cancel() {
            summary.cancelled = true;
            break;
        }
        let display_name = sanitize_name(source);
        progress(ProgressEvent {
            index: position + 1,
            total,
            name: display_name.clone(),
            phase: "start",
            size: fs::metadata(source).map(|meta| meta.len()).unwrap_or(0),
        });

        match import_one(vault_dir, master_key, source, &used_names, cancel) {
            Ok((entry, final_path, renamed)) => {
                index.entries.push(entry.clone());
                let outcome = index.save(vault_dir, master_key);
                if let Err(err) = apply_import_index_outcome(outcome, &mut index, &final_path) {
                    if err.code == "VAULT_RECOVERY_REQUIRED" {
                        return Err(err);
                    }
                    summary.failed.push(FailedFile {
                        name: display_name,
                        code: err.code,
                        message: err.message,
                    });
                    progress(ProgressEvent {
                        index: position + 1,
                        total,
                        name: entry.name,
                        phase: "failure",
                        size: entry.size,
                    });
                    continue;
                }
                used_names.insert(entry.name.clone());
                summary.succeeded.push(ImportedFile {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    size: entry.size,
                    renamed,
                });
                progress(ProgressEvent {
                    index: position + 1,
                    total,
                    name: entry.name,
                    phase: "success",
                    size: entry.size,
                });
            }
            Err(err) => {
                if err.code == "IMPORT_CANCELLED" {
                    summary.cancelled = true;
                    break;
                }
                summary.failed.push(FailedFile {
                    name: display_name.clone(),
                    code: err.code,
                    message: err.message,
                });
                progress(ProgressEvent {
                    index: position + 1,
                    total,
                    name: display_name,
                    phase: "failure",
                    size: 0,
                });
            }
        }
    }

    Ok(summary)
}

/// 校验库内全部文件：逐块认证并核对明文 SHA-256。
pub fn verify_all(
    vault_dir: &Path,
    master_key: &[u8; 32],
    cancel: &dyn Fn() -> bool,
) -> Result<VerifyReport, VaultError> {
    let _vault_lock = VaultLock::acquire(vault_dir)?;
    let index = Index::load(vault_dir, master_key)?;
    let mut report = VerifyReport {
        checked: 0,
        ok: Vec::new(),
        failed: Vec::new(),
        cancelled: false,
    };

    for entry in index.entries {
        if cancel() {
            report.cancelled = true;
            break;
        }
        report.checked += 1;
        let path = ciphertext_path(vault_dir, &entry.id);
        // 区分“密文被外部删除”与“密文认证失败”，两者都不暴露路径。
        let missing =
            matches!(fs::symlink_metadata(&path), Err(err) if err.kind() == ErrorKind::NotFound);
        let outcome = if missing {
            Err(VaultError::new("FILE_MISSING", "密文文件不存在。"))
        } else {
            verify_encrypted_file(&path, &entry, master_key)
        };
        match outcome {
            Ok(()) => report.ok.push(VerifyItem {
                id: entry.id,
                name: entry.name,
            }),
            Err(err) => report.failed.push(VerifyFailure {
                id: entry.id,
                name: entry.name,
                code: err.code,
                message: err.message,
            }),
        }
    }
    Ok(report)
}

fn rollback_delete_staging(staged: &Path, original: &Path) -> Result<(), VaultError> {
    open_regular_file(staged).map_err(|_| VaultError::recovery_required())?;
    match fs::symlink_metadata(original) {
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        _ => return Err(VaultError::recovery_required()),
    }
    commit_exclusive(staged, original).map_err(|_| VaultError::recovery_required())?;
    let parent = original
        .parent()
        .ok_or_else(VaultError::recovery_required)?;
    fsync_dir(parent).map_err(|_| VaultError::recovery_required())?;
    Ok(())
}

fn apply_delete_index_outcome(
    outcome: Result<IndexCommit, IndexSaveError>,
    staged: &Path,
    original: &Path,
) -> Result<(), VaultError> {
    match outcome {
        Ok(IndexCommit::Committed) => Ok(()),
        Err(err) if err.commit == IndexCommit::NotCommitted => {
            rollback_delete_staging(staged, original)?;
            fsync_dir(staged.parent().ok_or_else(VaultError::recovery_required)?)
                .map_err(|_| VaultError::recovery_required())?;
            Err(VaultError::delete_incomplete())
        }
        _ => Err(VaultError::recovery_required()),
    }
}

/// 已提交删除的物理清理失败不等于索引回滚；保留下次解锁可继续处理的现场。
fn cleanup_committed_delete(staged: &Path) -> Result<bool, VaultError> {
    match open_regular_file(staged) {
        Ok(file) => drop(file),
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(VaultError::recovery_required()),
    }
    match fs::remove_file(staged) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(_) => return Ok(true),
    }
    let parent = staged.parent().ok_or_else(VaultError::recovery_required)?;
    Ok(fsync_dir(parent).is_err())
}

/// 单文件直接删除：隔离重命名、原子提交索引，最后清理暂存密文。
pub fn delete_file(
    vault_dir: &Path,
    master_key: &[u8; 32],
    id: &str,
    cancel: &dyn Fn() -> bool,
) -> Result<DeletedFile, VaultError> {
    let _vault_lock = VaultLock::acquire(vault_dir).map_err(|err| {
        if err.code == "VAULT_BUSY" {
            VaultError::delete_busy()
        } else {
            err
        }
    })?;
    ensure_layout(vault_dir)?;
    let mut index = Index::load(vault_dir, master_key)?;
    recover_delete_files(vault_dir, &index)?;
    if !valid_file_id(id) {
        return Err(VaultError::new("FILE_NOT_FOUND", "找不到该文件。"));
    }
    let position = index
        .entries
        .iter()
        .position(|entry| entry.id == id)
        .ok_or_else(|| VaultError::new("FILE_NOT_FOUND", "找不到该文件。"))?;
    let original = ciphertext_path(vault_dir, id);
    let staged = delete_staging_path(vault_dir, id);
    let parent = original
        .parent()
        .ok_or_else(VaultError::delete_path_invalid)?;
    require_real_dir(parent).map_err(|_| VaultError::delete_path_invalid())?;
    match open_regular_file(&original) {
        Ok(file) => drop(file),
        Err(err) if err.kind() == ErrorKind::NotFound => return Err(file_missing()),
        Err(_) => return Err(VaultError::delete_path_invalid()),
    }
    match fs::symlink_metadata(&staged) {
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        _ => return Err(VaultError::recovery_required()),
    }
    if cancel() {
        return Err(VaultError::delete_cancelled());
    }

    // 进入临界区后不再响应取消；锁定通过任务许可等待到明确的安全退出点。
    commit_exclusive(&original, &staged)?;
    fsync_dir(parent).map_err(|_| VaultError::recovery_required())?;
    fsync_dir(&vault_dir.join(TMP_DIR)).map_err(|_| VaultError::recovery_required())?;

    index.entries.remove(position);
    let outcome = index.save(vault_dir, master_key);
    apply_delete_index_outcome(outcome, &staged, &original)?;
    Ok(DeletedFile {
        cleanup_pending: cleanup_committed_delete(&staged)?,
    })
}

/// 与前端一致的扩展名提取：只把最后一段非空后缀视为扩展名。
fn extension_of(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_string)
}

/// FR-32：校验重命名后的完整显示名称。
///
/// 扩展名必须保持一致，避免重命名把文件切换到其他类型分类；
/// 名称只用于索引展示，不参与任何路径拼接。
fn validate_rename_name(raw: &str, original: &str) -> Result<String, VaultError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(VaultError::file_name_empty());
    }
    if name.chars().count() > RENAME_NAME_MAX_CHARS {
        return Err(VaultError::file_name_too_long());
    }
    if name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name == "."
        || name == ".."
        || has_unsafe_display_chars(name)
    {
        return Err(VaultError::file_name_invalid());
    }
    if extension_of(name).map(|value| value.to_lowercase())
        != extension_of(original).map(|value| value.to_lowercase())
    {
        return Err(VaultError::file_name_extension_locked());
    }
    Ok(name.to_string())
}

/// 只修改索引中的显示名称。
///
/// - 不移动、不重新加密密文，文件 ID、大小、摘要与导入时间保持不变。
/// - 与导入共用库级排他锁，索引通过既有原子替换写入；失败不会改动密文。
/// - 索引提交状态不明时返回 `VAULT_RECOVERY_REQUIRED`，由会话撤销解密能力。
pub fn rename_file(
    vault_dir: &Path,
    master_key: &[u8; 32],
    id: &str,
    raw_name: &str,
) -> Result<FileListItem, VaultError> {
    if !valid_file_id(id) {
        return Err(VaultError::new("FILE_NOT_FOUND", "找不到该文件。"));
    }
    let _vault_lock = VaultLock::acquire(vault_dir)?;
    ensure_layout(vault_dir)?;
    let mut index = Index::load(vault_dir, master_key)?;
    let position = index
        .entries
        .iter()
        .position(|entry| entry.id == id)
        .ok_or_else(|| VaultError::new("FILE_NOT_FOUND", "找不到该文件。"))?;

    let original = index.entries[position].name.clone();
    let name = validate_rename_name(raw_name, &original)?;
    let normalized = name.to_lowercase();
    // 大小写不敏感的重名检查与 macOS 常见卷行为一致，也避免仅大小写不同的混淆。
    if index
        .entries
        .iter()
        .enumerate()
        .any(|(other, entry)| other != position && entry.name.to_lowercase() == normalized)
    {
        return Err(VaultError::file_name_exists());
    }

    if original == name {
        return Ok(index.entries.remove(position).into());
    }

    index.entries[position].name = name;
    match index.save(vault_dir, master_key) {
        Ok(IndexCommit::Committed) => Ok(index.entries.remove(position).into()),
        Err(err) if err.commit == IndexCommit::NotCommitted => Err(err.error),
        // rename 成功但目录同步未确认：名称可能已生效，必须走恢复流程。
        _ => Err(VaultError::recovery_required()),
    }
}

/// 设置或取消收藏。
///
/// - 收藏位保存在加密索引内，以稳定文件 ID 关联，不使用明文配置。
/// - 每次都在库级排他锁内重新加载索引后写入，避免覆盖其他文件的并发变更。
/// - 索引提交失败时磁盘保持旧状态；提交状态不明时走恢复流程。
pub fn set_favorite(
    vault_dir: &Path,
    master_key: &[u8; 32],
    id: &str,
    favorite: bool,
) -> Result<FileListItem, VaultError> {
    if !valid_file_id(id) {
        return Err(VaultError::new("FILE_NOT_FOUND", "找不到该文件。"));
    }
    let _vault_lock = VaultLock::acquire(vault_dir)?;
    ensure_layout(vault_dir)?;
    let mut index = Index::load(vault_dir, master_key)?;
    let position = index
        .entries
        .iter()
        .position(|entry| entry.id == id)
        .ok_or_else(|| VaultError::new("FILE_NOT_FOUND", "找不到该文件。"))?;

    if index.entries[position].favorite == favorite {
        return Ok(index.entries.remove(position).into());
    }

    index.entries[position].favorite = favorite;
    match index.save(vault_dir, master_key) {
        Ok(IndexCommit::Committed) => Ok(index.entries.remove(position).into()),
        Err(err) if err.commit == IndexCommit::NotCommitted => Err(err.error),
        _ => Err(VaultError::recovery_required()),
    }
}

/// 文件夹名称沿用文件显示名称的安全规则：非空、长度受限、无路径分隔符与控制字符。
fn validate_folder_name(raw: &str) -> Result<String, VaultError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(VaultError::folder_name_empty());
    }
    if name.chars().count() > FOLDER_NAME_MAX_CHARS {
        return Err(VaultError::folder_name_too_long());
    }
    if name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name == "."
        || name == ".."
        || has_unsafe_display_chars(name)
    {
        return Err(VaultError::folder_name_invalid());
    }
    Ok(name.to_string())
}

/// 生成不与现有文件夹冲突的稳定 ID；文件夹 ID 与文件 ID 使用同一随机格式。
fn allocate_folder_id(index: &Index) -> Result<String, VaultError> {
    for _ in 0..8 {
        let bytes = <[u8; FILE_ID_LEN]>::try_generate().map_err(|_| VaultError::internal())?;
        let id = hex_encode(&bytes);
        if !index.folders.iter().any(|folder| folder.id == id) {
            return Ok(id);
        }
    }
    Err(VaultError::internal())
}

fn map_folder_busy(error: VaultError) -> VaultError {
    if error.code == "VAULT_BUSY" {
        VaultError::folder_busy()
    } else {
        error
    }
}

/// 读取文件夹列表；只返回 ID 与名称，数量由前端按当前文件列表派生。
pub fn list_folders(
    vault_dir: &Path,
    master_key: &[u8; 32],
) -> Result<Vec<FolderItem>, VaultError> {
    let _vault_lock = VaultLock::acquire_shared(vault_dir)?;
    let mut folders = Index::load(vault_dir, master_key)?.folders;
    folders.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.id.cmp(&right.id))
    });
    Ok(folders.into_iter().map(FolderItem::from).collect())
}

/// 创建一级文件夹：名称同级唯一（大小写不敏感），ID 随机生成并写入加密索引。
pub fn create_folder(
    vault_dir: &Path,
    master_key: &[u8; 32],
    raw_name: &str,
) -> Result<FolderItem, VaultError> {
    let _vault_lock = VaultLock::acquire(vault_dir).map_err(map_folder_busy)?;
    ensure_layout(vault_dir)?;
    let mut index = Index::load(vault_dir, master_key)?;
    let name = validate_folder_name(raw_name)?;
    let normalized = name.to_lowercase();
    if index
        .folders
        .iter()
        .any(|folder| folder.name.to_lowercase() == normalized)
    {
        return Err(VaultError::folder_name_exists());
    }
    let id = allocate_folder_id(&index)?;
    index.folders.push(FolderEntry {
        id: id.clone(),
        name: name.clone(),
    });
    match index.save(vault_dir, master_key) {
        Ok(IndexCommit::Committed) => Ok(FolderItem { id, name }),
        Err(err) if err.commit == IndexCommit::NotCommitted => Err(err.error),
        _ => Err(VaultError::recovery_required()),
    }
}

/// 重命名文件夹：只改名称，文件 ID、密文与收藏状态保持不变。
pub fn rename_folder(
    vault_dir: &Path,
    master_key: &[u8; 32],
    id: &str,
    raw_name: &str,
) -> Result<FolderItem, VaultError> {
    if !valid_file_id(id) {
        return Err(VaultError::folder_not_found());
    }
    let _vault_lock = VaultLock::acquire(vault_dir).map_err(map_folder_busy)?;
    ensure_layout(vault_dir)?;
    let mut index = Index::load(vault_dir, master_key)?;
    let position = index
        .folders
        .iter()
        .position(|folder| folder.id == id)
        .ok_or_else(VaultError::folder_not_found)?;
    let name = validate_folder_name(raw_name)?;
    let normalized = name.to_lowercase();
    if index
        .folders
        .iter()
        .enumerate()
        .any(|(other, folder)| other != position && folder.name.to_lowercase() == normalized)
    {
        return Err(VaultError::folder_name_exists());
    }
    if index.folders[position].name == name {
        return Ok(index.folders.remove(position).into());
    }
    index.folders[position].name = name;
    match index.save(vault_dir, master_key) {
        Ok(IndexCommit::Committed) => Ok(index.folders.remove(position).into()),
        Err(err) if err.commit == IndexCommit::NotCommitted => Err(err.error),
        _ => Err(VaultError::recovery_required()),
    }
}

/// 删除空文件夹；仍有文件归属时拒绝，避免文件指向不存在的文件夹。
pub fn delete_folder(vault_dir: &Path, master_key: &[u8; 32], id: &str) -> Result<(), VaultError> {
    if !valid_file_id(id) {
        return Err(VaultError::folder_not_found());
    }
    let _vault_lock = VaultLock::acquire(vault_dir).map_err(map_folder_busy)?;
    ensure_layout(vault_dir)?;
    let mut index = Index::load(vault_dir, master_key)?;
    let position = index
        .folders
        .iter()
        .position(|folder| folder.id == id)
        .ok_or_else(VaultError::folder_not_found)?;
    if index
        .entries
        .iter()
        .any(|entry| entry.folder_id.as_deref() == Some(id))
    {
        return Err(VaultError::folder_not_empty());
    }
    index.folders.remove(position);
    match index.save(vault_dir, master_key) {
        Ok(IndexCommit::Committed) => Ok(()),
        Err(err) if err.commit == IndexCommit::NotCommitted => Err(err.error),
        _ => Err(VaultError::recovery_required()),
    }
}

/// 把一个或多个文件移动到指定文件夹；`folder_id` 为 None 表示移回根目录。
///
/// 整个批量移动共用一次索引保存：任何文件或目标文件夹不存在都整体失败，
/// 不会出现部分文件已移动、部分未移动的不一致状态。
pub fn move_files(
    vault_dir: &Path,
    master_key: &[u8; 32],
    ids: &[String],
    folder_id: Option<&str>,
) -> Result<Vec<FileListItem>, VaultError> {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        if !valid_file_id(id) {
            return Err(VaultError::new("FILE_NOT_FOUND", "找不到该文件。"));
        }
        if seen.insert(id.clone()) {
            unique.push(id.clone());
        }
    }
    if unique.is_empty() {
        return Ok(Vec::new());
    }

    let _vault_lock = VaultLock::acquire(vault_dir).map_err(map_folder_busy)?;
    ensure_layout(vault_dir)?;
    let mut index = Index::load(vault_dir, master_key)?;
    if let Some(target) = folder_id
        && !index.folders.iter().any(|folder| folder.id == target)
    {
        return Err(VaultError::folder_not_found());
    }

    // 先确认全部文件存在，再做任何写入，保证批量移动的原子性。
    let mut positions = Vec::with_capacity(unique.len());
    for id in &unique {
        let position = index
            .entries
            .iter()
            .position(|entry| &entry.id == id)
            .ok_or_else(|| VaultError::new("FILE_NOT_FOUND", "找不到该文件。"))?;
        positions.push(position);
    }

    let changed = positions
        .iter()
        .any(|&position| index.entries[position].folder_id.as_deref() != folder_id);
    if changed {
        for &position in &positions {
            index.entries[position].folder_id = folder_id.map(str::to_string);
        }
        match index.save(vault_dir, master_key) {
            Ok(IndexCommit::Committed) => {}
            Err(err) if err.commit == IndexCommit::NotCommitted => return Err(err.error),
            _ => return Err(VaultError::recovery_required()),
        }
    }

    Ok(positions
        .iter()
        .map(|&position| FileListItem::from(index.entries[position].clone()))
        .collect())
}

/// 导出写盘错误：空间不足沿用统一语义，其余指向用户所选位置而非保险库。
fn map_export_write_error(error: &io::Error) -> VaultError {
    if error.kind() == ErrorKind::StorageFull || error.raw_os_error() == Some(28) {
        VaultError::no_space()
    } else {
        VaultError::export_write_failed()
    }
}

/// 把索引中的一个条目解密并导出到 `target`。
///
/// 安全边界：
/// - 复用 `decrypt_stream` 的全部认证（头绑定、逐块 AEAD、截断、尾部、SHA-256），
///   任何一步失败都不会交付文件。
/// - 明文只写入按卷选择的私有暂存目录内的独占临时文件（0600），通过认证后才
///   以同卷 `RENAME_EXCL` 原子提交；目标已存在时内核层拒绝，绝不覆盖。
/// - 失败、取消、写盘错误都会删除临时文件与 sidecar；明文不经过 IPC，也不进入目标目录。
pub fn export_file(
    vault_dir: &Path,
    entry: &IndexEntry,
    master_key: &[u8; 32],
    target: &Path,
    forbidden_dirs: &[PathBuf],
    app_cache_dir: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<ExportedFile, VaultError> {
    let target = ExportTarget::open(vault_dir, target, forbidden_dirs)?;

    let cipher_path = ciphertext_path(vault_dir, &entry.id);
    let mut reader = match open_regular_file(&cipher_path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Err(file_missing()),
        Err(_) => return Err(VaultError::vault_unavailable()),
    };
    if cancel() {
        return Err(VaultError::export_cancelled());
    }

    let staging = StagingDir::resolve(app_cache_dir, &target)?;
    // 导出前轻量扫描：活动 sidecar 由锁自动跳过。
    let _ = staging.recover_orphans();
    let mut txn = ExportTxn::begin(&staging)?;

    let mut write_error: Option<VaultError> = None;
    let write_failed = std::cell::Cell::new(false);
    let decrypt_result = {
        let temp = txn.temp_mut();
        let mut sink = |chunk: &[u8]| {
            if write_error.is_some() {
                return;
            }
            if let Err(err) = temp.write_all(chunk) {
                write_failed.set(true);
                write_error = Some(map_export_write_error(&err));
            }
        };
        let cancelled = || cancel() || write_failed.get();
        decrypt_stream(&mut reader, entry, master_key, &cancelled, &mut sink)
    };

    if let Some(err) = write_error {
        return Err(txn.abort_error(err));
    }
    if let Err(err) = decrypt_result {
        let mapped = if err.code == "IMPORT_CANCELLED" {
            VaultError::export_cancelled()
        } else {
            err
        };
        return Err(txn.abort_error(mapped));
    }
    // 最后一块之后仍可能收到锁定发出的取消：此时不提交半成品。
    if cancel() {
        return Err(txn.abort_error(VaultError::export_cancelled()));
    }

    if let Err(err) = txn.sync_temp() {
        return Err(txn.abort_error(err));
    }

    // 进入提交临界区：此后不再检查取消。
    let outcome = txn.commit(&staging, &target);
    match outcome.commit {
        ExportCommit::Committed => Ok(ExportedFile {
            name: entry.name.clone(),
            size: entry.size,
            warning: if outcome.durability_confirmed {
                None
            } else {
                Some("文件已生成，但系统尚未确认持久化完成；请稍后检查目标位置。")
            },
        }),
        ExportCommit::NotCommitted => Err(outcome
            .error
            .unwrap_or_else(VaultError::export_write_failed)),
        ExportCommit::Unknown => Err(VaultError::export_commit_unknown()),
    }
}

/// 仅用于测试与后续导出：把单个密文文件解密为内存。
#[cfg(test)]
fn decrypt_to_vec(
    data: &[u8],
    entry: &IndexEntry,
    master_key: &[u8; 32],
) -> Result<Vec<u8>, VaultError> {
    let mut reader = io::Cursor::new(data);
    let mut out = Vec::new();
    decrypt_stream(&mut reader, entry, master_key, &|| false, &mut |block| {
        out.extend_from_slice(block);
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_key() -> Zeroizing<[u8; 32]> {
        Zeroizing::new(<[u8; 32]>::try_generate().expect("random key"))
    }

    fn temp_vault() -> (TempDir, PathBuf) {
        let temp = TempDir::new().expect("temp dir");
        let vault = temp.path().join("vault");
        fs::create_dir(&vault).expect("vault dir");
        (temp, vault)
    }

    fn encrypt_bytes(plain: &[u8], key: &[u8; 32]) -> (Vec<u8>, IndexEntry) {
        let mut reader = io::Cursor::new(plain);
        let mut out = Vec::new();
        let (_, sha) = encrypt_stream(
            &mut reader,
            plain.len() as u64,
            &mut out,
            key,
            [7u8; 16],
            &|| false,
        )
        .expect("encrypt");
        let entry = IndexEntry {
            id: hex_encode(&[7u8; 16]),
            name: "测试.bin".to_string(),
            size: plain.len() as u64,
            chunk_size: CHUNK_SIZE as u32,
            imported_at: 1,
            sha256: STANDARD.encode(sha),
            favorite: false,
            folder_id: None,
        };
        (out, entry)
    }

    fn import_paths(
        vault: &Path,
        key: &[u8; 32],
        paths: &[String],
        cancel: &dyn Fn() -> bool,
    ) -> ImportSummary {
        import_batch(vault, key, paths, cancel, &|_| {}).expect("import batch")
    }

    fn write_source(dir: &Path, name: &str, contents: &[u8]) -> String {
        let path = dir.join(name);
        fs::write(&path, contents).expect("write source");
        path.to_str().expect("utf8 path").to_string()
    }

    fn import_named(
        vault: &Path,
        temp: &TempDir,
        key: &[u8; 32],
        name: &str,
        body: &[u8],
    ) -> String {
        let source = write_source(temp.path(), name, body);
        import_paths(vault, key, &[source], &|| false)
            .succeeded
            .first()
            .expect("imported file")
            .id
            .clone()
    }

    #[test]
    fn rename_updates_index_without_touching_ciphertext() {
        let key = test_key();
        let (temp, vault) = temp_vault();
        let id = import_named(&vault, &temp, &key, "photo.jpg", b"jpeg-payload");

        let before = Index::load(&vault, &key).expect("index before");
        let original = before.entries[0].clone();
        let cipher_before = fs::read(ciphertext_path(&vault, &id)).expect("cipher before");

        let renamed = rename_file(&vault, &key, &id, "  假期 照片.jpg  ").expect("rename");
        assert_eq!(renamed.id, id);
        assert_eq!(renamed.name, "假期 照片.jpg");
        assert_eq!(renamed.size, original.size);
        assert_eq!(renamed.imported_at, original.imported_at);

        // 重新读取索引，确认名称已持久化且其余元数据不变。
        let after = Index::load(&vault, &key).expect("index after");
        assert_eq!(after.entries.len(), 1);
        assert_eq!(after.entries[0].name, "假期 照片.jpg");
        assert_eq!(after.entries[0].sha256, original.sha256);
        assert_eq!(
            fs::read(ciphertext_path(&vault, &id)).expect("cipher after"),
            cipher_before
        );
        assert_eq!(
            decrypt_to_vec(&cipher_before, &after.entries[0], &key).expect("decrypt"),
            b"jpeg-payload"
        );
    }

    #[test]
    fn rename_rejects_empty_and_blank_names() {
        let key = test_key();
        let (temp, vault) = temp_vault();
        let id = import_named(&vault, &temp, &key, "photo.jpg", b"x");

        for name in ["", "   ", "\t\n"] {
            let err = rename_file(&vault, &key, &id, name).expect_err("blank name");
            assert_eq!(err.code, "FILE_NAME_EMPTY", "{name:?}");
        }
        assert_eq!(
            Index::load(&vault, &key).expect("index").entries[0].name,
            "photo.jpg"
        );
    }

    #[test]
    fn rename_enforces_100_character_limit() {
        let key = test_key();
        let (temp, vault) = temp_vault();
        let id = import_named(&vault, &temp, &key, "photo.jpg", b"x");

        let max_name = format!("{}.jpg", "长".repeat(RENAME_NAME_MAX_CHARS - 4));
        assert_eq!(max_name.chars().count(), RENAME_NAME_MAX_CHARS);
        let renamed = rename_file(&vault, &key, &id, &max_name).expect("100 chars ok");
        assert_eq!(renamed.name, max_name);

        let too_long = format!("{}.jpg", "长".repeat(RENAME_NAME_MAX_CHARS - 3));
        let err = rename_file(&vault, &key, &id, &too_long).expect_err("101 chars");
        assert_eq!(err.code, "FILE_NAME_TOO_LONG");
        assert_eq!(
            Index::load(&vault, &key).expect("index").entries[0].name,
            max_name
        );
    }

    #[test]
    fn rename_rejects_illegal_characters() {
        let key = test_key();
        let (temp, vault) = temp_vault();
        let id = import_named(&vault, &temp, &key, "photo.jpg", b"x");

        for name in [
            "a/b.jpg",
            "a\\b.jpg",
            "a:b.jpg",
            "a\u{202E}b.jpg",
            "a\nb.jpg",
        ] {
            let err = rename_file(&vault, &key, &id, name).expect_err("illegal");
            assert_eq!(err.code, "FILE_NAME_INVALID", "{name:?}");
        }
        assert_eq!(
            Index::load(&vault, &key).expect("index").entries[0].name,
            "photo.jpg"
        );
    }

    #[test]
    fn rename_rejects_duplicate_names_ignoring_case() {
        let key = test_key();
        let (temp, vault) = temp_vault();
        let first = import_named(&vault, &temp, &key, "alpha.jpg", b"a");
        let second = import_named(&vault, &temp, &key, "beta.jpg", b"b");

        for name in ["alpha.jpg", "ALPHA.JPG"] {
            let err = rename_file(&vault, &key, &second, name).expect_err("duplicate");
            assert_eq!(err.code, "FILE_NAME_EXISTS");
            assert!(err.message.contains("已存在同名文件"));
        }
        let index = Index::load(&vault, &key).expect("index");
        assert_eq!(
            index
                .entries
                .iter()
                .find(|entry| entry.id == second)
                .expect("second")
                .name,
            "beta.jpg"
        );
        // 只修改自身名称大小写不算与其他文件重名。
        assert!(rename_file(&vault, &key, &first, "ALPHA.JPG").is_ok());
    }

    #[test]
    fn rename_keeps_extension_and_file_category() {
        let key = test_key();
        let (temp, vault) = temp_vault();
        let id = import_named(&vault, &temp, &key, "photo.jpg", b"x");

        for name in ["photo.png", "photo", "photo.jpg.txt"] {
            let err = rename_file(&vault, &key, &id, name).expect_err("extension changed");
            assert_eq!(err.code, "FILE_NAME_EXTENSION_LOCKED", "{name:?}");
        }
        assert!(rename_file(&vault, &key, &id, "photo.JPG").is_ok());
        assert_eq!(
            Index::load(&vault, &key).expect("index").entries[0].name,
            "photo.JPG"
        );

        // 原文件没有扩展名时也不能借重命名引入扩展名。
        let plain = import_named(&vault, &temp, &key, "notes", b"n");
        assert_eq!(
            rename_file(&vault, &key, &plain, "notes.txt")
                .expect_err("extension added")
                .code,
            "FILE_NAME_EXTENSION_LOCKED"
        );
        assert!(rename_file(&vault, &key, &plain, "我的笔记").is_ok());
    }

    #[test]
    fn failed_rename_preserves_index_and_ciphertext() {
        let key = test_key();
        let (temp, vault) = temp_vault();
        let id = import_named(&vault, &temp, &key, "keep.pdf", b"pdf-bytes");
        let index_before = fs::read(vault.join(INDEX_FILE)).expect("index file");
        let cipher_before = fs::read(ciphertext_path(&vault, &id)).expect("cipher");

        for (name, code) in [
            ("", "FILE_NAME_EMPTY"),
            ("keep", "FILE_NAME_EXTENSION_LOCKED"),
            ("bad/name.pdf", "FILE_NAME_INVALID"),
        ] {
            let err = rename_file(&vault, &key, &id, name).expect_err("must fail");
            assert_eq!(err.code, code, "{name:?}");
        }
        assert_eq!(
            rename_file(&vault, &key, "0000000000000000", "x.pdf")
                .expect_err("missing")
                .code,
            "FILE_NOT_FOUND"
        );
        assert_eq!(
            rename_file(&vault, &key, "../keep.pdf", "x.pdf")
                .expect_err("invalid id")
                .code,
            "FILE_NOT_FOUND"
        );

        // 失败的校验不写索引，也不触碰密文。
        assert_eq!(
            fs::read(vault.join(INDEX_FILE)).expect("index file"),
            index_before
        );
        assert_eq!(
            fs::read(ciphertext_path(&vault, &id)).expect("cipher"),
            cipher_before
        );
        let index = Index::load(&vault, &key).expect("index loads");
        assert_eq!(index.entries[0].name, "keep.pdf");
        assert_eq!(
            decrypt_to_vec(&cipher_before, &index.entries[0], &key).expect("decrypt"),
            b"pdf-bytes"
        );
    }

    #[test]
    fn small_file_roundtrip() {
        let key = test_key();
        let plain = b"hello haven".to_vec();
        let (ciphertext, entry) = encrypt_bytes(&plain, &key);
        let restored = decrypt_to_vec(&ciphertext, &entry, &key).expect("decrypt");
        assert_eq!(restored, plain);
    }

    #[test]
    fn multi_chunk_roundtrip() {
        let key = test_key();
        let plain: Vec<u8> = (0..(CHUNK_SIZE * 2 + 12345))
            .map(|index| (index % 251) as u8)
            .collect();
        let (ciphertext, entry) = encrypt_bytes(&plain, &key);
        assert_eq!(ciphertext.len(), HEADER_LEN + plain.len() + 3 * TAG_LEN);
        let restored = decrypt_to_vec(&ciphertext, &entry, &key).expect("decrypt");
        assert_eq!(restored, plain);
    }

    #[test]
    fn empty_file_roundtrip() {
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"", &key);
        assert_eq!(ciphertext.len(), HEADER_LEN + TAG_LEN);
        let restored = decrypt_to_vec(&ciphertext, &entry, &key).expect("decrypt");
        assert!(restored.is_empty());
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let key = test_key();
        let (mut ciphertext, entry) = encrypt_bytes(b"secret payload", &key);
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x40;
        assert_eq!(
            decrypt_to_vec(&ciphertext, &entry, &key)
                .expect_err("must fail")
                .code,
            "INTEGRITY_ERROR"
        );
    }

    #[test]
    fn truncated_last_chunk_fails() {
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"secret payload", &key);
        let truncated = &ciphertext[..ciphertext.len() - TAG_LEN];
        assert_eq!(
            decrypt_to_vec(truncated, &entry, &key)
                .expect_err("must fail")
                .code,
            "INTEGRITY_ERROR"
        );
    }

    #[test]
    fn header_only_file_fails() {
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"abc", &key);
        assert_eq!(
            decrypt_to_vec(&ciphertext[..HEADER_LEN], &entry, &key)
                .expect_err("must fail")
                .code,
            "INTEGRITY_ERROR"
        );
    }

    #[test]
    fn reordered_chunks_fail() {
        let key = test_key();
        let plain: Vec<u8> = (0..(CHUNK_SIZE * 2))
            .map(|index| (index % 97) as u8)
            .collect();
        let (mut ciphertext, entry) = encrypt_bytes(&plain, &key);
        let first = HEADER_LEN;
        let second = HEADER_LEN + CHUNK_SIZE + TAG_LEN;
        let mut reordered = ciphertext[..HEADER_LEN].to_vec();
        let chunk_a = ciphertext[first..first + CHUNK_SIZE + TAG_LEN].to_vec();
        let chunk_b = ciphertext[second..second + CHUNK_SIZE + TAG_LEN].to_vec();
        let last = ciphertext[second + CHUNK_SIZE + TAG_LEN..].to_vec();
        reordered.extend_from_slice(&chunk_b);
        reordered.extend_from_slice(&chunk_a);
        reordered.extend_from_slice(&last);
        ciphertext.truncate(0);
        assert_eq!(
            decrypt_to_vec(&reordered, &entry, &key)
                .expect_err("must fail")
                .code,
            "INTEGRITY_ERROR"
        );
    }

    #[test]
    fn tampered_header_is_rejected() {
        let key = test_key();
        let (mut ciphertext, mut entry) = encrypt_bytes(b"abc", &key);
        ciphertext[43] ^= 0x01;
        assert_eq!(
            decrypt_to_vec(&ciphertext, &entry, &key)
                .expect_err("must fail")
                .code,
            "INTEGRITY_ERROR"
        );

        let (ciphertext, _) = encrypt_bytes(b"abc", &key);
        entry.size = 4;
        assert_eq!(
            decrypt_to_vec(&ciphertext, &entry, &key)
                .expect_err("must fail")
                .code,
            "INTEGRITY_ERROR"
        );
    }

    #[test]
    fn extra_trailing_bytes_are_rejected() {
        let key = test_key();
        let (mut ciphertext, entry) = encrypt_bytes(b"abc", &key);
        ciphertext.push(0);
        assert_eq!(
            decrypt_to_vec(&ciphertext, &entry, &key)
                .expect_err("must fail")
                .code,
            "INTEGRITY_ERROR"
        );
    }

    #[test]
    fn file_ids_and_nonces_are_unique() {
        let key = test_key();
        let mut ids = HashSet::new();
        let mut nonces = HashSet::new();
        for _ in 0..16 {
            let id = <[u8; 16]>::try_generate().expect("random id");
            assert!(ids.insert(hex_encode(&id)));
            let mut reader = io::Cursor::new(b"same".to_vec());
            let mut out = Vec::new();
            let (header, _) =
                encrypt_stream(&mut reader, 4, &mut out, &key, id, &|| false).expect("encrypt");
            assert!(nonces.insert(header[24..43].to_vec()));
        }
    }

    #[test]
    fn index_roundtrip_protects_names() {
        let (_temp, vault) = temp_vault();
        let key = test_key();
        let index = Index {
            entries: vec![IndexEntry {
                id: hex_encode(&[3u8; 16]),
                name: "中文 文件.pdf".to_string(),
                size: 12,
                chunk_size: CHUNK_SIZE as u32,
                imported_at: 99,
                sha256: STANDARD.encode([9u8; 32]),
                favorite: false,
                folder_id: None,
            }],
            folders: Vec::new(),
        };
        index.save(&vault, &key).expect("save index");

        let raw = fs::read(vault.join(INDEX_FILE)).expect("read index");
        assert!(!raw.windows(3).any(|window| window == "中文".as_bytes()));
        let loaded = Index::load(&vault, &key).expect("load index");
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].name, "中文 文件.pdf");
    }

    #[test]
    fn favorite_persists_through_reload_rename_and_unfavorite() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("favorite-sources");
        fs::create_dir(&sources).expect("sources");
        let first = write_source(&sources, "a.txt", b"alpha");
        let second = write_source(&sources, "b.txt", b"beta");
        let summary = import_paths(&vault, &key, &[first, second], &|| false);
        assert_eq!(summary.succeeded.len(), 2);
        let first_id = summary.succeeded[0].id.clone();
        let second_id = summary.succeeded[1].id.clone();

        let updated = set_favorite(&vault, &key, &first_id, true).expect("favorite");
        assert!(updated.favorite);
        assert_eq!(updated.name, summary.succeeded[0].name);

        // 重新加载索引：收藏位持久化，其他条目不受影响。
        let index = Index::load(&vault, &key).expect("reload");
        assert!(
            index
                .entries
                .iter()
                .find(|entry| entry.id == first_id)
                .expect("first")
                .favorite
        );
        assert!(
            !index
                .entries
                .iter()
                .find(|entry| entry.id == second_id)
                .expect("second")
                .favorite
        );

        // 重命名只改名称，收藏位与 ID 保持。
        rename_file(&vault, &key, &first_id, "renamed.txt").expect("rename");
        let index = Index::load(&vault, &key).expect("reload after rename");
        let renamed = index
            .entries
            .iter()
            .find(|entry| entry.id == first_id)
            .expect("renamed");
        assert_eq!(renamed.name, "renamed.txt");
        assert!(renamed.favorite);

        let updated = set_favorite(&vault, &key, &first_id, false).expect("unfavorite");
        assert!(!updated.favorite);
        assert!(
            !Index::load(&vault, &key)
                .expect("reload after unfavorite")
                .entries
                .iter()
                .find(|entry| entry.id == first_id)
                .expect("first")
                .favorite
        );
    }

    #[test]
    fn favorite_rejects_unknown_id() {
        let (_temp, vault) = temp_vault();
        let key = test_key();
        let err = set_favorite(&vault, &key, "not-an-id", true).expect_err("unknown id");
        assert_eq!(err.code, "FILE_NOT_FOUND");
    }

    #[test]
    fn favorite_unknown_commit_requires_recovery() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("favorite-unknown");
        fs::create_dir(&sources).expect("sources");
        let source = write_source(&sources, "a.txt", b"alpha");
        let summary = import_paths(&vault, &key, &[source], &|| false);
        let id = summary.succeeded[0].id.clone();

        fault_injection::set_index_save_fsync_failure(true);
        let err = set_favorite(&vault, &key, &id, true).expect_err("unknown commit");
        fault_injection::set_index_save_fsync_failure(false);

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
    }

    #[test]
    fn index_entry_without_favorite_field_defaults_to_false() {
        let raw = r#"{"id":"abc","name":"a.txt","size":1,"chunkSize":65536,"importedAt":1,"sha256":"00"}"#;
        let entry: IndexEntry = serde_json::from_str(raw).expect("backward compatible");
        assert!(!entry.favorite);
    }

    #[test]
    fn index_without_folder_fields_remains_compatible() {
        // 旧索引既没有 folders，也没有条目上的 folderId，必须按“无文件夹、根目录”读取。
        let raw = r#"{"format":"haven-index","version":1,"entries":[{"id":"03030303030303030303030303030303","name":"a.txt","size":1,"chunkSize":65536,"importedAt":1,"sha256":"00"}]}"#;
        let parsed: IndexFile = serde_json::from_str(raw).expect("backward compatible");
        assert!(parsed.folders.is_empty());
        assert!(parsed.entries[0].folder_id.is_none());
    }

    #[test]
    fn folders_create_rename_delete_and_validate_names() {
        let (_temp, vault) = temp_vault();
        let key = test_key();

        assert_eq!(
            create_folder(&vault, &key, "   ").expect_err("empty").code,
            "FOLDER_NAME_EMPTY"
        );
        assert_eq!(
            create_folder(&vault, &key, "a/b").expect_err("slash").code,
            "FOLDER_NAME_INVALID"
        );

        let created = create_folder(&vault, &key, " 工作 ").expect("create");
        assert_eq!(created.name, "工作");
        assert_eq!(
            create_folder(&vault, &key, "工作")
                .expect_err("duplicate")
                .code,
            "FOLDER_NAME_EXISTS"
        );

        let renamed = rename_folder(&vault, &key, &created.id, "重要").expect("rename");
        assert_eq!(renamed.name, "重要");
        let folders = list_folders(&vault, &key).expect("list");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].id, created.id);
        assert_eq!(folders[0].name, "重要");

        delete_folder(&vault, &key, &created.id).expect("delete empty");
        assert!(list_folders(&vault, &key).expect("list empty").is_empty());
        assert_eq!(
            delete_folder(&vault, &key, &created.id)
                .expect_err("missing")
                .code,
            "FOLDER_NOT_FOUND"
        );
    }

    #[test]
    fn folder_delete_rejects_non_empty_and_move_back_to_root() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("move-sources");
        fs::create_dir(&sources).expect("sources");
        let source = write_source(&sources, "note.txt", b"note");
        let summary = import_paths(&vault, &key, &[source], &|| false);
        let file_id = summary.succeeded[0].id.clone();
        let folder = create_folder(&vault, &key, "资料").expect("create folder");

        let moved = move_files(
            &vault,
            &key,
            std::slice::from_ref(&file_id),
            Some(&folder.id),
        )
        .expect("move into folder");
        assert_eq!(moved[0].folder_id.as_deref(), Some(folder.id.as_str()));
        assert_eq!(
            delete_folder(&vault, &key, &folder.id)
                .expect_err("non-empty")
                .code,
            "FOLDER_NOT_EMPTY"
        );

        let rooted =
            move_files(&vault, &key, std::slice::from_ref(&file_id), None).expect("move to root");
        assert!(rooted[0].folder_id.is_none());
        // 必须显式返回 folderId: null，前端才能区分“移回根目录”与“未返回该字段”。
        let serialized = serde_json::to_value(&rooted[0]).expect("serialize");
        assert!(serialized["folderId"].is_null());
        delete_folder(&vault, &key, &folder.id).expect("delete after empty");
    }

    #[test]
    fn move_files_keeps_favorites_and_handles_batch_failure_atomically() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("batch-sources");
        fs::create_dir(&sources).expect("sources");
        let first = write_source(&sources, "a.txt", b"alpha");
        let second = write_source(&sources, "b.txt", b"beta");
        let summary = import_paths(&vault, &key, &[first, second], &|| false);
        let first_id = summary.succeeded[0].id.clone();
        let second_id = summary.succeeded[1].id.clone();
        set_favorite(&vault, &key, &first_id, true).expect("favorite");
        let folder = create_folder(&vault, &key, "归档").expect("folder");

        let moved = move_files(
            &vault,
            &key,
            &[first_id.clone(), second_id.clone(), first_id.clone()],
            Some(&folder.id),
        )
        .expect("batch move");
        assert_eq!(moved.len(), 2);
        assert!(
            moved
                .iter()
                .all(|item| item.folder_id.as_deref() == Some(folder.id.as_str()))
        );

        // 文件夹重命名不影响文件归属与收藏。
        rename_folder(&vault, &key, &folder.id, "长期归档").expect("rename folder");
        let index = Index::load(&vault, &key).expect("reload");
        let first_entry = index
            .entries
            .iter()
            .find(|entry| entry.id == first_id)
            .expect("first");
        assert_eq!(first_entry.folder_id.as_deref(), Some(folder.id.as_str()));
        assert!(first_entry.favorite);

        // 任一文件不存在时整批失败，已存在的文件也不会被移动。
        assert_eq!(
            move_files(
                &vault,
                &key,
                &[second_id.clone(), "not-an-id".to_string()],
                None
            )
            .expect_err("invalid id")
            .code,
            "FILE_NOT_FOUND"
        );
        let index = Index::load(&vault, &key).expect("reload after failure");
        assert_eq!(
            index
                .entries
                .iter()
                .find(|entry| entry.id == second_id)
                .expect("second")
                .folder_id
                .as_deref(),
            Some(folder.id.as_str())
        );

        // 未知目标文件夹同样整体失败。
        assert_eq!(
            move_files(
                &vault,
                &key,
                std::slice::from_ref(&second_id),
                Some("00000000000000000000000000000000")
            )
            .expect_err("unknown folder")
            .code,
            "FOLDER_NOT_FOUND"
        );
        assert_eq!(
            move_files(
                &vault,
                &key,
                std::slice::from_ref(&second_id),
                Some("not-a-folder-id")
            )
            .expect_err("invalid folder")
            .code,
            "FOLDER_NOT_FOUND"
        );
    }

    #[test]
    fn move_files_unknown_commit_requires_recovery() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("unknown-sources");
        fs::create_dir(&sources).expect("sources");
        let source = write_source(&sources, "x.txt", b"x");
        let id = import_paths(&vault, &key, &[source], &|| false).succeeded[0]
            .id
            .clone();
        let folder = create_folder(&vault, &key, "目标").expect("folder");

        fault_injection::set_index_save_fsync_failure(true);
        let err = move_files(&vault, &key, &[id], Some(&folder.id)).expect_err("unknown commit");
        fault_injection::set_index_save_fsync_failure(false);

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
    }

    #[test]
    fn tampered_index_fails_to_load() {
        let (_temp, vault) = temp_vault();
        let key = test_key();
        Index {
            entries: vec![IndexEntry {
                id: hex_encode(&[3u8; 16]),
                name: "a.txt".to_string(),
                size: 1,
                chunk_size: CHUNK_SIZE as u32,
                imported_at: 1,
                sha256: STANDARD.encode([0u8; 32]),
                favorite: false,
                folder_id: None,
            }],
            folders: Vec::new(),
        }
        .save(&vault, &key)
        .expect("save");

        let mut raw = fs::read(vault.join(INDEX_FILE)).expect("read");
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        fs::write(vault.join(INDEX_FILE), raw).expect("write");
        assert_eq!(
            Index::load(&vault, &key).expect_err("must fail").code,
            "INDEX_INVALID"
        );
    }

    #[test]
    fn missing_index_with_ciphertext_is_not_rebuilt() {
        let (_temp, vault) = temp_vault();
        let key = test_key();
        ensure_layout(&vault).expect("layout");
        let path = ciphertext_path(&vault, &hex_encode(&[4u8; 16]));
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, b"orphan").expect("write");

        assert_eq!(
            Index::load(&vault, &key).expect_err("must fail").code,
            "INDEX_MISSING"
        );
        assert!(path.exists());
    }

    #[test]
    fn import_multiple_files_and_dedup_names() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("sources");
        fs::create_dir(&sources).expect("sources");
        let paths = vec![
            write_source(&sources, "a.txt", b"one"),
            write_source(&sources, "b.txt", b"two"),
            write_source(&sources, "a.txt", b"three"),
        ];

        let summary = import_paths(&vault, &key, &paths, &|| false);
        assert_eq!(summary.succeeded.len(), 3);
        assert!(summary.failed.is_empty());

        let index = Index::load(&vault, &key).expect("load");
        let names: Vec<&str> = index
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "a (1).txt"]);
        let mut ids = HashSet::new();
        for entry in &index.entries {
            assert!(ids.insert(entry.id.clone()));
            let path = ciphertext_path(&vault, &entry.id);
            assert!(path.is_file());
        }
        // 原文件始终保留（测试中该源文件被同一测试覆盖写为 three）。
        assert!(sources.join("a.txt").exists());
    }

    #[test]
    fn chinese_file_name_is_preserved() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("sources");
        fs::create_dir(&sources).expect("sources");
        let path = write_source(&sources, "春 日 照片 2026.jpg", b"data");

        let summary = import_paths(&vault, &key, &[path], &|| false);
        assert_eq!(summary.succeeded[0].name, "春 日 照片 2026.jpg");
        assert_eq!(
            Index::load(&vault, &key).expect("load").entries[0].name,
            "春 日 照片 2026.jpg"
        );
    }

    #[test]
    fn missing_source_fails_without_index_entry() {
        let (_temp, vault) = temp_vault();
        let key = test_key();
        let summary = import_paths(
            &vault,
            &key,
            &["/definitely/missing/file.txt".to_string()],
            &|| false,
        );
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.failed[0].code, "SOURCE_MISSING");
        assert!(Index::load(&vault, &key).expect("load").entries.is_empty());
        assert!(scan_ciphertext_ids(&vault).expect("scan").is_empty());
    }

    #[test]
    fn directory_import_recurses_and_rejects_symlinks() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let root = temp.path().join("drop");
        fs::create_dir_all(root.join("nested/deeper")).expect("dirs");
        write_source(&root, "top.txt", b"top");
        write_source(&root.join("nested"), "mid.txt", b"mid");
        write_source(&root.join("nested/deeper"), "bottom.txt", b"bottom");

        let outside = temp.path().join("outside");
        fs::create_dir(&outside).expect("outside");
        write_source(&outside, "secret.txt", b"secret");
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt"))
            .expect("file symlink");
        std::os::unix::fs::symlink(&outside, root.join("link-dir")).expect("dir symlink");

        let summary = import_paths(
            &vault,
            &key,
            &[root.to_str().expect("utf8").to_string()],
            &|| false,
        );

        let mut names: Vec<&str> = summary
            .succeeded
            .iter()
            .map(|file| file.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, vec!["bottom.txt", "mid.txt", "top.txt"]);
        assert_eq!(summary.failed.len(), 2);
        assert!(
            summary
                .failed
                .iter()
                .all(|file| file.code == "SOURCE_UNSUPPORTED")
        );

        let index = Index::load(&vault, &key).expect("load");
        assert_eq!(index.entries.len(), 3);
        // 符号链接指向的目录外文件不得被导入。
        assert!(!index.entries.iter().any(|entry| entry.name == "secret.txt"));
        assert_eq!(scan_ciphertext_ids(&vault).expect("scan").len(), 3);
    }

    #[test]
    fn mixed_files_and_directories_import_together() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let root = temp.path().join("drop");
        fs::create_dir_all(root.join("sub")).expect("dirs");
        // 文件夹外的普通文件，与文件夹一起拖入。
        let loose = write_source(temp.path(), "loose.txt", b"loose");
        write_source(&root.join("sub"), "nested.txt", b"nested");

        let summary = import_paths(
            &vault,
            &key,
            &[loose, root.to_str().expect("utf8").to_string()],
            &|| false,
        );

        assert_eq!(summary.succeeded.len(), 2);
        assert!(summary.failed.is_empty());
        assert_eq!(Index::load(&vault, &key).expect("load").entries.len(), 2);
    }

    #[test]
    fn empty_directory_import_changes_nothing() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let root = temp.path().join("empty");
        fs::create_dir_all(root.join("also-empty")).expect("dirs");

        let summary = import_paths(
            &vault,
            &key,
            &[root.to_str().expect("utf8").to_string()],
            &|| false,
        );

        assert!(summary.succeeded.is_empty());
        assert!(summary.failed.is_empty());
        assert!(!summary.cancelled);
        assert!(Index::load(&vault, &key).expect("load").entries.is_empty());
        assert!(scan_ciphertext_ids(&vault).expect("scan").is_empty());
    }

    #[test]
    fn directory_duplicate_names_follow_existing_rename_rule() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let root = temp.path().join("dup");
        fs::create_dir_all(root.join("sub")).expect("dirs");
        write_source(&root, "same.txt", b"one");
        write_source(&root.join("sub"), "same.txt", b"two");

        let summary = import_paths(
            &vault,
            &key,
            &[root.to_str().expect("utf8").to_string()],
            &|| false,
        );

        assert_eq!(summary.succeeded.len(), 2);
        let index = Index::load(&vault, &key).expect("load");
        let mut names: Vec<&str> = index
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, vec!["same (1).txt", "same.txt"]);
    }

    #[test]
    fn unreadable_directory_and_file_do_not_abort_batch() {
        use std::os::unix::fs::PermissionsExt;

        let (temp, vault) = temp_vault();
        let key = test_key();
        let root = temp.path().join("mixed");
        fs::create_dir_all(root.join("locked-dir")).expect("dirs");
        write_source(&root, "ok.txt", b"ok");
        write_source(&root.join("locked-dir"), "hidden.txt", b"hidden");
        let locked_file = root.join("locked.txt");
        fs::write(&locked_file, b"locked").expect("locked file");

        fs::set_permissions(root.join("locked-dir"), fs::Permissions::from_mode(0o000))
            .expect("chmod dir");
        fs::set_permissions(&locked_file, fs::Permissions::from_mode(0o000)).expect("chmod file");

        let summary = import_paths(
            &vault,
            &key,
            &[root.to_str().expect("utf8").to_string()],
            &|| false,
        );

        // 恢复正常权限，保证临时目录可以清理。
        fs::set_permissions(root.join("locked-dir"), fs::Permissions::from_mode(0o755))
            .expect("restore dir");
        fs::set_permissions(&locked_file, fs::Permissions::from_mode(0o644)).expect("restore file");

        assert_eq!(summary.succeeded.len(), 1);
        assert_eq!(summary.succeeded[0].name, "ok.txt");
        assert!(summary.failed.len() >= 2);
        assert!(
            summary
                .failed
                .iter()
                .all(|file| file.code == "SOURCE_UNREADABLE")
        );
        assert_eq!(Index::load(&vault, &key).expect("load").entries.len(), 1);
    }

    #[test]
    fn cancel_before_directory_scan_imports_nothing() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let root = temp.path().join("drop");
        fs::create_dir(&root).expect("dir");
        write_source(&root, "a.txt", b"a");

        let summary = import_paths(
            &vault,
            &key,
            &[root.to_str().expect("utf8").to_string()],
            &|| true,
        );

        assert!(summary.cancelled);
        assert!(summary.succeeded.is_empty());
        assert!(Index::load(&vault, &key).expect("load").entries.is_empty());
        assert!(scan_ciphertext_ids(&vault).expect("scan").is_empty());
    }

    #[test]
    fn cancel_mid_import_leaves_no_entry_or_partial_file() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("sources");
        fs::create_dir(&sources).expect("sources");
        let big: Vec<u8> = vec![7u8; CHUNK_SIZE * 2];
        let path = write_source(&sources, "big.bin", &big);

        let calls = std::cell::Cell::new(0usize);
        let cancel = || {
            calls.set(calls.get() + 1);
            calls.get() >= 2
        };
        let summary = import_paths(&vault, &key, &[path], &cancel);
        assert!(summary.cancelled);
        assert!(summary.succeeded.is_empty());
        assert!(Index::load(&vault, &key).expect("load").entries.is_empty());
        assert!(scan_ciphertext_ids(&vault).expect("scan").is_empty());
        let tmp_entries = fs::read_dir(vault.join(TMP_DIR)).expect("tmp").count();
        assert_eq!(tmp_entries, 0);
    }

    #[test]
    fn orphan_ciphertext_is_preserved_on_prepare() {
        let (_temp, vault) = temp_vault();
        let key = test_key();
        ensure_layout(&vault).expect("layout");
        // 模拟已完成初始化的库：索引存在，但本次导入在提交密文后、更新索引前中断。
        Index::default().save(&vault, &key).expect("empty index");
        let path = ciphertext_path(&vault, &hex_encode(&[5u8; 16]));
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, b"partial").expect("write");

        // 索引是唯一事实来源：未被索引登记的密文不会被当作正常文件展示。
        // 但也不能在启动时自动删除——库目录可能被备份回放或多实例挂载，
        // 贸然删除会造成不可恢复的数据丢失。
        prepare_vault(&vault, &key).expect("prepare vault");
        assert!(path.exists());
        assert!(Index::load(&vault, &key).expect("load").entries.is_empty());
    }

    #[test]
    fn verify_reports_tampered_file() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("sources");
        fs::create_dir(&sources).expect("sources");
        let path = write_source(&sources, "ok.txt", b"verify me");
        let summary = import_paths(&vault, &key, &[path], &|| false);
        assert_eq!(summary.succeeded.len(), 1);

        let report = verify_all(&vault, &key, &|| false).expect("verify");
        assert_eq!(report.ok.len(), 1);
        assert!(report.failed.is_empty());

        let entry = Index::load(&vault, &key).expect("load").entries.remove(0);
        let cipher_path = ciphertext_path(&vault, &entry.id);
        let mut raw = fs::read(&cipher_path).expect("read");
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        fs::write(&cipher_path, raw).expect("write");

        let report = verify_all(&vault, &key, &|| false).expect("verify");
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].code, "INTEGRITY_ERROR");
    }

    #[test]
    fn index_save_failure_rolls_back_ciphertext() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        ensure_layout(&vault).expect("layout");
        let sources = temp.path().join("sources");
        fs::create_dir(&sources).expect("sources");
        let path = write_source(&sources, "rollback.txt", b"payload");

        // 预先创建库级锁文件：稍后把库根目录设为只读时，锁仍可打开已有文件，
        // 从而让失败点落在索引临时文件的创建上，而不是库级锁的获取上。
        fs::write(vault.join(".haven.lock"), b"").expect("lock file");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 让索引临时文件无法在库根目录创建，同时保持 tmp/ 与 files/ 可写。
            fs::set_permissions(&vault, fs::Permissions::from_mode(0o500)).expect("chmod");
        }

        let source_paths = vec![path];
        let summary = import_paths(&vault, &key, &source_paths, &|| false);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&vault, fs::Permissions::from_mode(0o700)).expect("chmod back");
        }

        assert_eq!(summary.succeeded.len(), 0);
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.failed[0].code, "DISK_ERROR");
        assert!(scan_ciphertext_ids(&vault).expect("scan").is_empty());
        assert!(!vault.join(INDEX_FILE).exists());
    }

    #[test]
    fn import_does_not_overwrite_existing_ciphertext() {
        let (_temp, vault) = temp_vault();
        ensure_layout(&vault).expect("layout");
        let id = hex_encode(&[6u8; 16]);
        let path = ciphertext_path(&vault, &id);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, b"existing").expect("write");

        // allocate_file 必须跳过已存在的路径，返回不同 ID。
        let (_, new_id, _) = allocate_file(&vault).expect("allocate");
        assert_ne!(new_id, id);
    }

    #[test]
    fn name_sanitization_rejects_control_characters() {
        assert_eq!(sanitize_name(Path::new("/tmp/ok.txt")), "ok.txt");
        assert_eq!(sanitize_name(Path::new("/tmp/bad\u{7}.txt")), "未命名文件");
        // 方向标记与双向控制符同样可能伪造扩展名显示。
        assert_eq!(sanitize_name(Path::new("/tmp/a\u{200F}.txt")), "未命名文件");
        assert_eq!(sanitize_name(Path::new("/tmp/a\u{202E}.txt")), "未命名文件");
        assert_eq!(sanitize_name(Path::new("/tmp/a\u{061C}.txt")), "未命名文件");
    }

    #[test]
    fn missing_files_dir_is_not_treated_as_empty() {
        let (_temp, vault) = temp_vault();
        let key = test_key();
        // 无法确认库内是否已有密文时，绝不能返回空索引。
        assert_eq!(
            Index::load(&vault, &key).expect_err("must fail").code,
            "VAULT_UNAVAILABLE"
        );
    }

    #[test]
    fn verify_reports_missing_ciphertext_separately() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("sources");
        fs::create_dir(&sources).expect("sources");
        let path = write_source(&sources, "gone.txt", b"delete me");
        let summary = import_paths(&vault, &key, &[path], &|| false);
        assert_eq!(summary.succeeded.len(), 1);

        let entry = Index::load(&vault, &key).expect("load").entries.remove(0);
        fs::remove_file(ciphertext_path(&vault, &entry.id)).expect("remove ciphertext");

        let report = verify_all(&vault, &key, &|| false).expect("verify");
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].code, "FILE_MISSING");
    }

    #[test]
    fn commit_does_not_overwrite_existing_target() {
        let (_temp, vault) = temp_vault();
        let dir = vault.join(FILES_DIR).join("aa");
        fs::create_dir_all(&dir).expect("dir");
        let target = dir.join("existing.hvn");
        fs::write(&target, b"old").expect("target");
        let staging = vault.join("staging.part");
        fs::write(&staging, b"new").expect("staging");

        let err = commit_exclusive(&staging, &target).expect_err("must fail");
        assert_eq!(err.code, "FILE_EXISTS");
        assert_eq!(fs::read(&target).expect("read"), b"old");
        // 提交失败不移动临时文件，由调用方负责清理。
        assert!(staging.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn concurrent_commit_allows_exactly_one_winner() {
        use std::sync::{Arc, Barrier};

        let (_temp, vault) = temp_vault();
        let dir = vault.join(FILES_DIR).join("aa");
        fs::create_dir_all(&dir).expect("dir");
        let target = dir.join("race.hvn");
        let staging_a = vault.join("a.part");
        let staging_b = vault.join("b.part");
        fs::write(&staging_a, b"AAA").expect("a");
        fs::write(&staging_b, b"BBB").expect("b");

        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for staging in [staging_a, staging_b] {
            let barrier = Arc::clone(&barrier);
            let target = target.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                commit_exclusive(&staging, &target)
            }));
        }
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("join"))
            .collect();
        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "并发提交必须恰好一个成功"
        );
        let loser_code = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .map(|err| err.code);
        assert_eq!(loser_code, Some("FILE_EXISTS"));
        let content = fs::read(&target).expect("read");
        assert!(content == b"AAA" || content == b"BBB");
    }

    fn store_ciphertext(vault: &Path, entry: &IndexEntry, ciphertext: &[u8]) -> PathBuf {
        let path = ciphertext_path(vault, &entry.id);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, ciphertext).expect("write ciphertext");
        path
    }

    fn snapshot_files(root: &Path) -> Vec<(PathBuf, u64)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if kind.is_dir() {
                    stack.push(path);
                } else {
                    let len = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                    out.push((path, len));
                }
            }
        }
        out.sort();
        out
    }

    /// 构造带合法 IHDR 的最小 PNG 头部，用于预览尺寸测试。
    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut out = PNG_MAGIC.to_vec();
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        // 位深、颜色类型、压缩、过滤、隔行扫描与占位 CRC。
        out.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        out
    }

    /// 构造 APP0 + SOF 的最小 JPEG 头部，SOF 不在固定偏移，用于验证逐段解析。
    fn jpeg_bytes(marker: u8, width: u16, height: u16) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        // APP0（JFIF）段：长度 16 包含自身 2 字节，内容共 14 字节。
        out.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        out.extend_from_slice(b"JFIF\0");
        out.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
        // SOF 段：精度 1 字节，随后是高、宽与 3 个分量。
        out.extend_from_slice(&[0xFF, marker, 0x00, 0x11, 0x08]);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
        out
    }

    #[test]
    fn preview_returns_original_png_bytes() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let plain = png_bytes(900, 600);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "photo.png".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        let restored = preview_image(&vault, &entry, &key, &|| false).expect("preview");
        assert_eq!(restored, plain);
    }

    #[test]
    fn preview_returns_original_jpeg_bytes() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let plain = jpeg_bytes(0xC0, 900, 600);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "photo.JPEG".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        let restored = preview_image(&vault, &entry, &key, &|| false).expect("preview");
        assert_eq!(restored, plain);
    }

    #[test]
    fn preview_rejects_non_image_entry() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (ciphertext, mut entry) = encrypt_bytes(b"pdf-ish", &key);
        entry.name = "document.pdf".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_image(&vault, &entry, &key, &|| false).expect_err("reject");
        assert_eq!(err.code, "PREVIEW_UNSUPPORTED");
    }

    #[test]
    fn preview_rejects_oversized_entry_before_reading() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (ciphertext, mut entry) = encrypt_bytes(b"small", &key);
        entry.name = "huge.png".to_string();
        entry.size = PREVIEW_MAX_BYTES + 1;
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_image(&vault, &entry, &key, &|| false).expect_err("reject");
        assert_eq!(err.code, "PREVIEW_TOO_LARGE");
    }

    /// 生成可直接解码的 PNG，用于缩略图测试。
    fn encoded_png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([24, 120, 200, 255]));
        let mut out = Cursor::new(Vec::new());
        image
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    /// 生成可直接解码的 JPEG，用于缩略图测试。
    fn encoded_jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 90, 40]));
        let mut out = Cursor::new(Vec::new());
        image
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .expect("encode jpeg");
        out.into_inner()
    }

    /// 构造带 ISO BMFF 头的伪 MP4 字节（只需通过容器头校验）。
    fn fake_iso_video(size: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; size.max(16)];
        bytes[4..8].copy_from_slice(b"ftyp");
        bytes[8..12].copy_from_slice(b"isom");
        bytes
    }

    /// 构造带 EBML 头的伪 WebM 字节。
    fn fake_webm_video() -> Vec<u8> {
        let mut bytes = WEBM_MAGIC.to_vec();
        bytes.extend_from_slice(&[0u8; 24]);
        bytes
    }

    #[test]
    fn preview_video_returns_authenticated_bytes_without_plaintext_files() {
        let key = test_key();
        let (temp, vault) = temp_vault();
        let plain = fake_iso_video(4096);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "clip.mp4".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        let before = snapshot_files(&vault);

        let bytes = preview_video(&vault, &entry, &key, &|| false).expect("video preview");

        assert_eq!(bytes, plain);
        assert_eq!(snapshot_files(&vault), before, "视频预览不得写入磁盘");
        assert!(!temp.path().join(TMP_DIR).exists());
    }

    #[test]
    fn preview_video_accepts_mov_m4v_and_webm() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let mut legacy_mov = vec![0u8; 64];
        legacy_mov[4..8].copy_from_slice(b"moov");

        for (name, plain) in [
            ("clip.mov", fake_iso_video(64)),
            ("clip.m4v", fake_iso_video(64)),
            ("legacy.mov", legacy_mov),
            ("clip.webm", fake_webm_video()),
        ] {
            let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
            entry.name = name.to_string();
            store_ciphertext(&vault, &entry, &ciphertext);
            let bytes = preview_video(&vault, &entry, &key, &|| false).expect("video preview");
            assert_eq!(bytes, plain, "容器 {name} 应可预览");
        }
    }

    #[test]
    fn preview_video_rejects_non_video_and_invalid_container() {
        let key = test_key();
        let (_temp, vault) = temp_vault();

        let (ciphertext, mut entry) = encrypt_bytes(b"plain text", &key);
        entry.name = "notes.txt".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        let err = preview_video(&vault, &entry, &key, &|| false).expect_err("non video");
        assert_eq!(err.code, "PREVIEW_UNSUPPORTED");

        let mut broken = vec![0u8; 64];
        broken[0..4].copy_from_slice(b"JUNK");
        let (ciphertext, mut entry) = encrypt_bytes(&broken, &key);
        entry.name = "broken.mp4".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        let err = preview_video(&vault, &entry, &key, &|| false).expect_err("broken mp4");
        assert_eq!(err.code, "PREVIEW_INVALID");
    }

    #[test]
    fn preview_video_rejects_oversized_entry_before_reading() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (ciphertext, mut entry) = encrypt_bytes(&fake_iso_video(64), &key);
        entry.name = "large.mp4".to_string();
        entry.size = VIDEO_PREVIEW_MAX_BYTES + 1;
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_video(&vault, &entry, &key, &|| false).expect_err("reject");
        assert_eq!(err.code, "PREVIEW_TOO_LARGE");
        assert!(err.message.contains("64 MB"));
    }

    #[test]
    fn preview_video_detects_tampered_ciphertext() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (mut ciphertext, mut entry) = encrypt_bytes(&fake_iso_video(256), &key);
        entry.name = "clip.mp4".to_string();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0xff;
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_video(&vault, &entry, &key, &|| false).expect_err("tampered");
        assert_eq!(err.code, "INTEGRITY_ERROR");
    }

    #[test]
    fn thumbnail_is_bounded_png_and_leaves_no_plaintext() {
        let key = test_key();
        let (temp, vault) = temp_vault();
        let plain = encoded_png(640, 480);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "photo.png".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        let before = snapshot_files(&vault);

        let thumb = preview_thumbnail(&vault, &entry, &key, &|| false).expect("thumbnail");

        assert!(thumb.starts_with(PNG_MAGIC));
        let decoded = image::load_from_memory_with_format(&thumb, image::ImageFormat::Png)
            .expect("decode thumbnail");
        assert_eq!((decoded.width(), decoded.height()), (320, 240));
        assert_eq!(snapshot_files(&vault), before, "缩略图不得写入磁盘");
        assert!(!temp.path().join(TMP_DIR).exists());
    }

    #[test]
    fn thumbnail_scales_jpeg_and_preserves_aspect_ratio() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let plain = encoded_jpeg(300, 600);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "portrait.jpg".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        let thumb = preview_thumbnail(&vault, &entry, &key, &|| false).expect("thumbnail");
        let decoded = image::load_from_memory_with_format(&thumb, image::ImageFormat::Png)
            .expect("decode thumbnail");
        assert_eq!((decoded.width(), decoded.height()), (160, 320));
    }

    #[test]
    fn thumbnail_rejects_non_image_and_invalid_content() {
        let key = test_key();
        let (_temp, vault) = temp_vault();

        let (ciphertext, mut entry) = encrypt_bytes(b"not-an-image", &key);
        entry.name = "notes.txt".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        let err = preview_thumbnail(&vault, &entry, &key, &|| false).expect_err("non image");
        assert_eq!(err.code, "PREVIEW_UNSUPPORTED");

        let mut broken = PNG_MAGIC.to_vec();
        broken.extend_from_slice(b"corrupted pixels");
        let (ciphertext, mut entry) = encrypt_bytes(&broken, &key);
        entry.name = "broken.png".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        let err = preview_thumbnail(&vault, &entry, &key, &|| false).expect_err("broken png");
        assert_eq!(err.code, "PREVIEW_INVALID");
    }

    #[test]
    fn thumbnail_rejects_oversized_entry_before_reading() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (ciphertext, mut entry) = encrypt_bytes(b"small", &key);
        entry.name = "huge.png".to_string();
        entry.size = PREVIEW_MAX_BYTES + 1;
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_thumbnail(&vault, &entry, &key, &|| false).expect_err("reject");
        assert_eq!(err.code, "PREVIEW_TOO_LARGE");
    }

    #[test]
    fn thumbnail_detects_tampered_ciphertext() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (mut ciphertext, mut entry) = encrypt_bytes(&encoded_png(64, 64), &key);
        entry.name = "photo.png".to_string();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0xff;
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_thumbnail(&vault, &entry, &key, &|| false).expect_err("tampered");
        assert_eq!(err.code, "INTEGRITY_ERROR");
    }

    #[test]
    fn png_dimensions_reads_ihdr() {
        assert_eq!(png_dimensions(&png_bytes(900, 600)), Some((900, 600)));
    }

    #[test]
    fn png_dimensions_rejects_malformed_headers() {
        // 截断到 IHDR 之前。
        assert_eq!(png_dimensions(&PNG_MAGIC[..8]), None);
        assert_eq!(png_dimensions(&png_bytes(900, 600)[..23]), None);
        // 首块不是 IHDR。
        let mut bytes = png_bytes(10, 10);
        bytes[12..16].copy_from_slice(b"IDAT");
        assert_eq!(png_dimensions(&bytes), None);
        // 块长度异常。
        let mut bytes = png_bytes(10, 10);
        bytes[8..12].copy_from_slice(&12u32.to_be_bytes());
        assert_eq!(png_dimensions(&bytes), None);
        // 缺失 PNG 签名。
        let mut bytes = png_bytes(10, 10);
        bytes[0] = 0;
        assert_eq!(png_dimensions(&bytes), None);
    }

    #[test]
    fn jpeg_dimensions_parses_sof_after_other_segments() {
        assert_eq!(
            jpeg_dimensions(&jpeg_bytes(0xC0, 900, 600)),
            Some((900, 600))
        );
        // 渐进式 SOF2 与带 0xFF 填充的标记同样可以解析。
        assert_eq!(
            jpeg_dimensions(&jpeg_bytes(0xC2, 1024, 768)),
            Some((1024, 768))
        );
        let mut filled = vec![0xFF, 0xD8, 0xFF, 0xFF];
        filled.extend_from_slice(&jpeg_bytes(0xC2, 320, 240)[2..]);
        assert_eq!(jpeg_dimensions(&filled), Some((320, 240)));
    }

    #[test]
    fn jpeg_dimensions_rejects_truncated_and_abnormal_markers() {
        let full = jpeg_bytes(0xC0, 900, 600);
        for cut in [2usize, 3, 4, 6, 20, 24, 30] {
            assert_eq!(
                jpeg_dimensions(&full[..cut.min(full.len())]),
                None,
                "cut={cut}"
            );
        }
        // 只有 SOI，没有任何段。
        assert_eq!(jpeg_dimensions(&[0xFF, 0xD8]), None);
        // SOS 出现在 SOF 之前。
        let sos = [0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x08, 1, 2, 3, 4];
        assert_eq!(jpeg_dimensions(&sos), None);
        // 段长度超出缓冲区。
        let mut bad = full.clone();
        bad[4] = 0xFF;
        bad[5] = 0xFF;
        assert_eq!(jpeg_dimensions(&bad), None);
        // 标记失步。
        let mut bad = full.clone();
        bad[2] = 0x00;
        assert_eq!(jpeg_dimensions(&bad), None);
    }

    #[test]
    fn preview_accepts_pixel_budget_boundary() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let plain = png_bytes(4000, 4000);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "boundary.png".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        assert_eq!(
            preview_image(&vault, &entry, &key, &|| false).expect("preview"),
            plain
        );
    }

    #[test]
    fn preview_accepts_max_single_edge() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let plain = png_bytes(8192, 1);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "edge.png".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        assert!(preview_image(&vault, &entry, &key, &|| false).is_ok());
    }

    fn assert_dimensions_rejected(name: &str, plain: Vec<u8>, expected: &str) {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = name.to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        let err = preview_image(&vault, &entry, &key, &|| false).expect_err("reject");
        assert_eq!(err.code, expected);
    }

    #[test]
    fn preview_rejects_pixel_budget_overflow() {
        // 4001 * 4000 = 16,004,000，仅比上限多 4000 像素。
        assert_dimensions_rejected("over.png", png_bytes(4001, 4000), "PREVIEW_TOO_LARGE");
    }

    #[test]
    fn preview_rejects_oversized_single_edge() {
        assert_dimensions_rejected("wide.png", png_bytes(8193, 1), "PREVIEW_TOO_LARGE");
        assert_dimensions_rejected("tall.png", png_bytes(1, 8193), "PREVIEW_TOO_LARGE");
    }

    #[test]
    fn preview_rejects_zero_dimensions() {
        assert_dimensions_rejected("empty.png", png_bytes(0, 600), "PREVIEW_INVALID");
        assert_dimensions_rejected("empty.jpg", jpeg_bytes(0xC0, 0, 600), "PREVIEW_INVALID");
    }

    #[test]
    fn preview_rejects_malformed_headers() {
        let mut truncated = png_bytes(900, 600);
        truncated.truncate(20);
        assert_dimensions_rejected("short.png", truncated, "PREVIEW_INVALID");
        let mut bad_jpeg = jpeg_bytes(0xC0, 900, 600);
        bad_jpeg.truncate(26);
        assert_dimensions_rejected("short.jpg", bad_jpeg, "PREVIEW_INVALID");
    }

    #[test]
    fn preview_rejects_huge_jpeg_without_integer_overflow() {
        // 65535 * 65535 远超 u32 像素计数，必须按 u64 计算并拒绝。
        assert_dimensions_rejected(
            "huge.jpg",
            jpeg_bytes(0xC0, u16::MAX, u16::MAX),
            "PREVIEW_TOO_LARGE",
        );
    }

    #[test]
    fn preview_detects_tampered_ciphertext() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let mut plain = PNG_MAGIC.to_vec();
        plain.extend_from_slice(&[4, 4, 4, 4]);
        let (mut ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "broken.png".to_string();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x01;
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_image(&vault, &entry, &key, &|| false).expect_err("tampered");
        assert_eq!(err.code, "INTEGRITY_ERROR");
    }

    #[test]
    fn preview_detects_truncated_ciphertext() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let mut plain = PNG_MAGIC.to_vec();
        plain.extend_from_slice(&[1, 1, 1, 1]);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "short.png".to_string();
        let truncated = &ciphertext[..ciphertext.len() - 4];
        store_ciphertext(&vault, &entry, truncated);

        let err = preview_image(&vault, &entry, &key, &|| false).expect_err("truncated");
        assert_eq!(err.code, "INTEGRITY_ERROR");
    }

    #[test]
    fn preview_reports_missing_ciphertext() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (_ciphertext, mut entry) = encrypt_bytes(b"missing", &key);
        entry.name = "gone.png".to_string();

        let err = preview_image(&vault, &entry, &key, &|| false).expect_err("missing");
        assert_eq!(err.code, "FILE_MISSING");
    }

    #[test]
    fn preview_rejects_extension_magic_mismatch() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (ciphertext, mut entry) = encrypt_bytes(b"not really a png", &key);
        entry.name = "fake.png".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_image(&vault, &entry, &key, &|| false).expect_err("mismatch");
        assert_eq!(err.code, "PREVIEW_INVALID");
    }

    #[test]
    fn preview_leaves_no_plaintext_files() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let plain = png_bytes(64, 64);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "memory-only.png".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        let before = snapshot_files(&vault);
        let restored = preview_image(&vault, &entry, &key, &|| false).expect("preview");
        assert_eq!(restored, plain);
        assert_eq!(snapshot_files(&vault), before);
        assert!(!vault.join(TMP_DIR).exists());
    }

    fn pdf_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        bytes.extend_from_slice(b"trailer\n<< /Root 1 0 R /Size 3 >>\n%%EOF\n");
        bytes
    }

    #[test]
    fn preview_pdf_returns_original_bytes_without_plaintext_files() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let plain = pdf_bytes();
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "report.PDF".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        let before = snapshot_files(&vault);
        let restored = preview_pdf(&vault, &entry, &key, &|| false).expect("preview");
        assert_eq!(restored, plain);
        assert_eq!(snapshot_files(&vault), before);
        assert!(!vault.join(TMP_DIR).exists());
    }

    #[test]
    fn preview_pdf_accepts_header_within_first_1024_bytes() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let mut plain = vec![b' '; 512];
        plain.extend_from_slice(&pdf_bytes());
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "offset.pdf".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        assert_eq!(
            preview_pdf(&vault, &entry, &key, &|| false).expect("preview"),
            plain
        );
    }

    #[test]
    fn preview_pdf_rejects_non_pdf_entry() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (ciphertext, mut entry) = encrypt_bytes(&pdf_bytes(), &key);
        entry.name = "report.txt".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_pdf(&vault, &entry, &key, &|| false).expect_err("reject");
        assert_eq!(err.code, "PREVIEW_UNSUPPORTED");
    }

    #[test]
    fn preview_pdf_rejects_oversized_entry_before_reading() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (ciphertext, mut entry) = encrypt_bytes(b"small", &key);
        entry.name = "huge.pdf".to_string();
        entry.size = PREVIEW_MAX_BYTES + 1;
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = preview_pdf(&vault, &entry, &key, &|| false).expect_err("reject");
        assert_eq!(err.code, "PREVIEW_TOO_LARGE");
        assert!(err.message.contains("PDF"));
    }

    #[test]
    fn preview_pdf_rejects_missing_or_late_header() {
        let key = test_key();
        let (_temp, vault) = temp_vault();

        let (ciphertext, mut entry) = encrypt_bytes(b"not a pdf at all", &key);
        entry.name = "fake.pdf".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        assert_eq!(
            preview_pdf(&vault, &entry, &key, &|| false)
                .expect_err("missing header")
                .code,
            "PREVIEW_INVALID"
        );

        // 文件头落在前 1024 字节之外：不符合 PDF 规范，也不接受。
        let mut late = vec![b' '; 1024];
        late.extend_from_slice(b"%PDF-1.4\n");
        let (ciphertext, mut entry) = encrypt_bytes(&late, &key);
        entry.name = "late.pdf".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);
        assert_eq!(
            preview_pdf(&vault, &entry, &key, &|| false)
                .expect_err("late header")
                .code,
            "PREVIEW_INVALID"
        );
    }

    #[test]
    fn preview_pdf_detects_tampered_and_missing_ciphertext() {
        let key = test_key();
        let (_temp, vault) = temp_vault();
        let (mut ciphertext, mut entry) = encrypt_bytes(&pdf_bytes(), &key);
        entry.name = "broken.pdf".to_string();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x01;
        store_ciphertext(&vault, &entry, &ciphertext);

        assert_eq!(
            preview_pdf(&vault, &entry, &key, &|| false)
                .expect_err("tampered")
                .code,
            "INTEGRITY_ERROR"
        );

        let (_ciphertext, mut missing) = encrypt_bytes(&pdf_bytes(), &key);
        missing.name = "gone.pdf".to_string();
        // 使用不同的文件 ID，确保对应的密文确实不存在。
        missing.id = hex_encode(&[8u8; FILE_ID_LEN]);
        assert_eq!(
            preview_pdf(&vault, &missing, &key, &|| false)
                .expect_err("missing")
                .code,
            "FILE_MISSING"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preview_rejects_symlink_ciphertext() {
        use std::os::unix::fs::symlink;

        let key = test_key();
        let (temp, vault) = temp_vault();
        let mut plain = PNG_MAGIC.to_vec();
        plain.extend_from_slice(&[1]);
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "linked.png".to_string();

        let outside = temp.path().join("outside.hvn");
        fs::write(&outside, &ciphertext).expect("outside");
        let link = ciphertext_path(&vault, &entry.id);
        fs::create_dir_all(link.parent().expect("parent")).expect("dir");
        symlink(&outside, &link).expect("symlink");

        let err = preview_image(&vault, &entry, &key, &|| false).expect_err("symlink");
        assert_eq!(err.code, "VAULT_UNAVAILABLE");
    }

    #[test]
    fn import_into_missing_vault_is_rejected() {
        let temp = TempDir::new().expect("temp dir");
        let key = test_key();
        let missing = temp.path().join("missing-vault");

        let err = import_batch(&missing, &key, &[], &|| false, &|_| {}).expect_err("must reject");
        assert_eq!(err.code, "VAULT_UNAVAILABLE");
        assert!(!missing.exists());
    }

    fn export_dir(temp: &TempDir) -> PathBuf {
        let dir = temp.path().join("export-out");
        fs::create_dir(&dir).expect("export dir");
        dir
    }

    /// 测试专用应用缓存目录（与保险库同卷但独立目录）。
    fn export_cache(vault: &Path) -> PathBuf {
        vault
            .parent()
            .map(|parent| parent.join("export-cache"))
            .unwrap_or_else(|| vault.join("export-cache"))
    }

    fn assert_no_export_temp(dir: &Path) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let name = entry.expect("entry").file_name();
            assert!(
                !name.to_string_lossy().starts_with(".haven-export-"),
                "导出临时文件未被清理"
            );
        }
    }

    #[test]
    fn export_restores_original_bytes_with_unicode_target() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let plain = b"export me \x00\x01\x02".to_vec();
        let (ciphertext, mut entry) = encrypt_bytes(&plain, &key);
        entry.name = "春日 报告 2026.txt".to_string();
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        let target = out.join("导出的 文件 (1).txt");
        let result = export_file(
            &vault,
            &entry,
            &key,
            &target,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect("export");

        assert_eq!(result.name, "春日 报告 2026.txt");
        assert_eq!(result.size, plain.len() as u64);
        assert_eq!(fs::read(&target).expect("read export"), plain);
        assert_no_export_temp(&out);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "导出文件应仅本人可读写");
        }
    }

    #[test]
    fn export_empty_file_creates_empty_target() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"", &key);
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        let target = out.join("empty.txt");
        let result = export_file(
            &vault,
            &entry,
            &key,
            &target,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect("export");
        assert_eq!(result.size, 0);
        assert_eq!(fs::metadata(&target).expect("meta").len(), 0);
    }

    #[test]
    fn export_multi_chunk_file_matches_source() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let plain: Vec<u8> = (0..(CHUNK_SIZE * 2 + 12345))
            .map(|index| (index % 251) as u8)
            .collect();
        let (ciphertext, entry) = encrypt_bytes(&plain, &key);
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        let target = out.join("large.bin");
        export_file(
            &vault,
            &entry,
            &key,
            &target,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect("export");
        assert_eq!(fs::read(&target).expect("read"), plain);
        assert_no_export_temp(&out);
    }

    #[cfg(unix)]
    #[test]
    fn export_uses_staging_not_target_directory() {
        use std::os::unix::fs::PermissionsExt;

        let (temp, vault) = temp_vault();
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"staging proof", &key);
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        let target = out.join("proof.txt");
        let cache = export_cache(&vault);
        export_file(&vault, &entry, &key, &target, &[], &cache, &|| false).expect("export");

        // 目标目录只含最终文件，不出现任何 .haven-export-* 明文临时文件。
        let names: Vec<String> = fs::read_dir(&out)
            .expect("out")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["proof.txt".to_string()]);
        assert_eq!(
            fs::metadata(&target).expect("meta").permissions().mode() & 0o777,
            0o600
        );

        // 启动卷暂存目录已建立，且导出完成后没有残留。
        let staging = cache.join("export-staging");
        assert!(staging.is_dir());
        let leftovers: Vec<String> = fs::read_dir(&staging)
            .expect("staging")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".haven-export-"))
            .collect();
        assert!(leftovers.is_empty(), "暂存目录存在导出残留: {leftovers:?}");
    }

    #[test]
    fn export_rejects_tampered_ciphertext_and_cleans_temp() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let (mut ciphertext, entry) = encrypt_bytes(b"secret payload", &key);
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x40;
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        let target = out.join("tampered.txt");
        let err = export_file(
            &vault,
            &entry,
            &key,
            &target,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect_err("must fail");
        assert_eq!(err.code, "INTEGRITY_ERROR");
        assert!(!target.exists(), "认证失败不得交付文件");
        assert_no_export_temp(&out);
    }

    #[test]
    fn export_reports_missing_ciphertext() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let (_, entry) = encrypt_bytes(b"gone", &key);
        // 不写入密文，模拟密文被外部删除。
        let out = export_dir(&temp);
        let target = out.join("missing.txt");
        let err = export_file(
            &vault,
            &entry,
            &key,
            &target,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect_err("must fail");
        assert_eq!(err.code, "FILE_MISSING");
        assert!(!target.exists());
        assert_no_export_temp(&out);
    }

    #[test]
    fn export_never_overwrites_existing_target() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"new data", &key);
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        let target = out.join("keep.txt");
        fs::write(&target, b"original").expect("existing target");

        let err = export_file(
            &vault,
            &entry,
            &key,
            &target,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect_err("must reject");
        assert_eq!(err.code, "FILE_EXISTS");
        assert_eq!(fs::read(&target).expect("read"), b"original");
        assert_no_export_temp(&out);
    }

    #[cfg(unix)]
    #[test]
    fn export_rejects_symlink_target_without_following_it() {
        use std::os::unix::fs::symlink;

        let (temp, vault) = temp_vault();
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"through link", &key);
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, b"outside").expect("outside");
        let link = out.join("link.txt");
        symlink(&outside, &link).expect("symlink");

        let err = export_file(
            &vault,
            &entry,
            &key,
            &link,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect_err("must reject");
        assert_eq!(err.code, "FILE_EXISTS");
        assert_eq!(fs::read(&outside).expect("read outside"), b"outside");
        assert_no_export_temp(&out);
    }

    #[test]
    fn export_rejects_target_inside_vault_or_protected_dir() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"protected", &key);
        store_ciphertext(&vault, &entry, &ciphertext);

        let inside_vault = vault.join("leak.txt");
        let err = export_file(
            &vault,
            &entry,
            &key,
            &inside_vault,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect_err("vault path");
        assert_eq!(err.code, "EXPORT_TARGET_INVALID");
        assert!(!inside_vault.exists());

        let protected = temp.path().join("app-cache");
        fs::create_dir(&protected).expect("protected dir");
        let inside_protected = protected.join("leak.txt");
        let err = export_file(
            &vault,
            &entry,
            &key,
            &inside_protected,
            std::slice::from_ref(&protected),
            &export_cache(&vault),
            &|| false,
        )
        .expect_err("protected path");
        assert_eq!(err.code, "EXPORT_TARGET_INVALID");
        assert!(!inside_protected.exists());
    }

    #[test]
    fn export_rejects_relative_and_directory_targets() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"paths", &key);
        store_ciphertext(&vault, &entry, &ciphertext);

        let err = export_file(
            &vault,
            &entry,
            &key,
            Path::new("relative.txt"),
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect_err("relative");
        assert_eq!(err.code, "EXPORT_TARGET_INVALID");

        let out = export_dir(&temp);
        let dir_target = out.join("sub");
        fs::create_dir(&dir_target).expect("dir target");
        let err = export_file(
            &vault,
            &entry,
            &key,
            &dir_target,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect_err("directory");
        assert_eq!(err.code, "EXPORT_TARGET_INVALID");
    }

    #[test]
    fn export_cancel_leaves_no_partial_or_temp_file() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let plain: Vec<u8> = vec![3u8; CHUNK_SIZE * 2];
        let (ciphertext, entry) = encrypt_bytes(&plain, &key);
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        let target = out.join("cancelled.bin");
        let calls = std::cell::Cell::new(0usize);
        let cancel = || {
            calls.set(calls.get() + 1);
            calls.get() >= 2
        };
        let err = export_file(
            &vault,
            &entry,
            &key,
            &target,
            &[],
            &export_cache(&vault),
            &cancel,
        )
        .expect_err("must cancel");
        assert_eq!(err.code, "EXPORT_CANCELLED");
        assert!(!target.exists());
        assert_no_export_temp(&out);
    }

    #[cfg(unix)]
    #[test]
    fn export_write_failure_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;

        let (temp, vault) = temp_vault();
        let key = test_key();
        let (ciphertext, entry) = encrypt_bytes(b"no write", &key);
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        fs::set_permissions(&out, fs::Permissions::from_mode(0o500)).expect("chmod");
        let target = out.join("denied.txt");
        let err = export_file(
            &vault,
            &entry,
            &key,
            &target,
            &[],
            &export_cache(&vault),
            &|| false,
        )
        .expect_err("must fail");
        fs::set_permissions(&out, fs::Permissions::from_mode(0o700)).expect("chmod back");

        assert_eq!(err.code, "EXPORT_WRITE_FAILED");
        assert!(!target.exists());
        assert_no_export_temp(&out);
    }

    #[test]
    fn export_abort_cleanup_failure_reports_cleanup_pending() {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let (mut ciphertext, entry) = encrypt_bytes(b"secret payload", &key);
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x40;
        store_ciphertext(&vault, &entry, &ciphertext);

        let out = export_dir(&temp);
        let target = out.join("tampered.txt");
        let cache = export_cache(&vault);
        crate::export_txn::fault_injection::set_temp_unlink_failure(true);
        let err = export_file(&vault, &entry, &key, &target, &[], &cache, &|| false)
            .expect_err("must fail");
        crate::export_txn::fault_injection::set_temp_unlink_failure(false);

        assert_eq!(err.code, "EXPORT_CLEANUP_PENDING");
        assert!(!target.exists(), "认证失败不得交付文件");
        assert_no_export_temp(&out);

        // 暂存目录必须同时保留 temp 与 sidecar，等待恢复流程继续清理。
        let staging = cache.join("export-staging");
        let names: Vec<String> = fs::read_dir(&staging)
            .expect("staging")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".haven-export-"))
            .collect();
        assert_eq!(names.len(), 2, "temp 与 sidecar 都必须保留");
        assert!(names.iter().any(|name| name.ends_with(".part")));
        assert!(names.iter().any(|name| name.ends_with(".lock")));
    }

    fn delete_fixture(names: &[&str]) -> (TempDir, PathBuf, Zeroizing<[u8; 32]>, Vec<IndexEntry>) {
        let (temp, vault) = temp_vault();
        let key = test_key();
        let sources = temp.path().join("delete-sources");
        fs::create_dir(&sources).expect("sources");
        let paths: Vec<String> = names
            .iter()
            .map(|name| write_source(&sources, name, name.as_bytes()))
            .collect();
        let summary = import_paths(&vault, &key, &paths, &|| false);
        assert_eq!(summary.succeeded.len(), names.len());
        assert!(summary.failed.is_empty());
        let entries = Index::load(&vault, &key).expect("load").entries;
        (temp, vault, key, entries)
    }

    #[test]
    fn delete_removes_index_and_ciphertext_but_not_source() {
        let (temp, vault, key, entries) = delete_fixture(&["delete-me.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);

        let result = delete_file(&vault, &key, id, &|| false).expect("delete");

        assert!(!result.cleanup_pending);
        assert!(!original.exists());
        assert!(!staged.exists());
        assert!(Index::load(&vault, &key).expect("load").entries.is_empty());
        assert!(temp.path().join("delete-sources/delete-me.txt").exists());
    }

    #[test]
    fn delete_preserves_other_files_and_their_plaintext() {
        let (_temp, vault, key, entries) = delete_fixture(&["keep.txt", "remove.txt"]);
        let remove = entries
            .iter()
            .find(|entry| entry.name == "remove.txt")
            .expect("remove entry")
            .clone();
        let keep = entries
            .iter()
            .find(|entry| entry.name == "keep.txt")
            .expect("keep entry")
            .clone();
        let keep_path = ciphertext_path(&vault, &keep.id);

        delete_file(&vault, &key, &remove.id, &|| false).expect("delete");

        let index = Index::load(&vault, &key).expect("load");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].id, keep.id);
        assert_eq!(
            decrypt_to_vec(
                &fs::read(&keep_path).expect("read kept ciphertext"),
                &keep,
                &key,
            )
            .expect("decrypt kept file"),
            b"keep.txt"
        );
        assert!(
            verify_all(&vault, &key, &|| false)
                .expect("verify")
                .failed
                .is_empty()
        );
    }

    #[test]
    fn delete_rejects_invalid_and_unknown_ids_without_changes() {
        let (_temp, vault, key, entries) = delete_fixture(&["stay.txt"]);
        let original = ciphertext_path(&vault, &entries[0].id);

        for id in ["../secret", "00000000000000000000000000000000"] {
            let err = delete_file(&vault, &key, id, &|| false).expect_err("reject id");
            assert_eq!(err.code, "FILE_NOT_FOUND");
            assert!(original.exists());
            assert_eq!(Index::load(&vault, &key).expect("load").entries.len(), 1);
        }
    }

    #[test]
    fn delete_reports_missing_ciphertext_and_keeps_index() {
        let (_temp, vault, key, entries) = delete_fixture(&["missing.txt"]);
        let id = &entries[0].id;
        fs::remove_file(ciphertext_path(&vault, id)).expect("remove ciphertext");

        let err = delete_file(&vault, &key, id, &|| false).expect_err("missing");

        assert_eq!(err.code, "FILE_MISSING");
        assert_eq!(Index::load(&vault, &key).expect("load").entries.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn delete_rejects_symlink_original_and_staging_paths() {
        use std::os::unix::fs::symlink;

        let (temp, vault, key, entries) = delete_fixture(&["linked.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);
        let outside = temp.path().join("outside.hvn");
        let ciphertext_before = fs::read(&original).expect("ciphertext before");
        fs::write(&outside, b"outside").expect("outside");
        fs::rename(&original, &outside).expect("move original");
        symlink(&outside, &original).expect("original symlink");

        let err = delete_file(&vault, &key, id, &|| false).expect_err("reject original");
        assert_eq!(err.code, "DELETE_PATH_INVALID");
        assert!(
            fs::symlink_metadata(&original)
                .expect("metadata")
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&outside).expect("outside"), ciphertext_before);

        fs::remove_file(&original).expect("remove link");
        fs::rename(&outside, &original).expect("restore original");
        fs::write(&outside, b"outside").expect("outside again");
        symlink(&outside, &staged).expect("staging symlink");
        let before = fs::read(&original).expect("original before");
        let err = delete_file(&vault, &key, id, &|| false).expect_err("reject staging");
        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert_eq!(fs::read(&original).expect("original after"), before);
        assert_eq!(fs::read(&outside).expect("outside after"), b"outside");
    }

    #[test]
    fn delete_rejects_special_original_file() {
        let (_temp, vault, key, entries) = delete_fixture(&["special.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        fs::remove_file(&original).expect("remove original");
        fs::create_dir(&original).expect("directory at ciphertext path");

        let err = delete_file(&vault, &key, id, &|| false).expect_err("reject special");

        assert_eq!(err.code, "DELETE_PATH_INVALID");
        assert!(original.is_dir());
        assert_eq!(Index::load(&vault, &key).expect("load").entries.len(), 1);
    }

    #[test]
    fn delete_staging_conflict_preserves_both_files() {
        let (_temp, vault, key, entries) = delete_fixture(&["conflict.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);
        fs::write(&staged, b"preserve staging").expect("staging conflict");
        let before = fs::read(&original).expect("original before");

        let err = delete_file(&vault, &key, id, &|| false).expect_err("conflict");

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert_eq!(fs::read(&original).expect("original after"), before);
        assert_eq!(
            fs::read(&staged).expect("staged after"),
            b"preserve staging"
        );
    }

    #[cfg(unix)]
    #[test]
    fn delete_quarantine_rename_failure_leaves_original_indexed() {
        use std::os::unix::fs::PermissionsExt;

        let (_temp, vault, key, entries) = delete_fixture(&["quarantine.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);
        let parent = original.parent().expect("parent").to_path_buf();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).expect("chmod");

        let err = delete_file(&vault, &key, id, &|| false).expect_err("rename must fail");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("chmod back");

        assert_eq!(err.code, "DISK_ERROR");
        assert!(original.is_file());
        assert!(!staged.exists());
        assert_eq!(Index::load(&vault, &key).expect("load").entries.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn delete_index_not_committed_rolls_back_to_original() {
        use std::os::unix::fs::PermissionsExt;

        let (_temp, vault, key, entries) = delete_fixture(&["rollback.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);
        let before = fs::read(&original).expect("original before");
        fs::set_permissions(&vault, fs::Permissions::from_mode(0o500)).expect("chmod");

        let err = delete_file(&vault, &key, id, &|| false).expect_err("save must fail");
        fs::set_permissions(&vault, fs::Permissions::from_mode(0o700)).expect("chmod back");

        assert_eq!(err.code, "DELETE_INCOMPLETE");
        assert_eq!(fs::read(&original).expect("rolled back"), before);
        assert!(!staged.exists());
        assert_eq!(Index::load(&vault, &key).expect("load").entries.len(), 1);
    }

    #[test]
    fn delete_rollback_failure_preserves_both_copies() {
        let (_temp, vault) = temp_vault();
        ensure_layout(&vault).expect("layout");
        let id = hex_encode(&[0x51; 16]);
        let original = ciphertext_path(&vault, &id);
        let staged = delete_staging_path(&vault, &id);
        fs::create_dir_all(original.parent().expect("parent")).expect("parent");
        fs::write(&original, b"original").expect("original");
        fs::write(&staged, b"staged").expect("staged");

        let err = apply_delete_index_outcome(
            Err(IndexSaveError::not_committed(VaultError::disk())),
            &staged,
            &original,
        )
        .expect_err("rollback must fail");

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert_eq!(fs::read(&original).expect("original"), b"original");
        assert_eq!(fs::read(&staged).expect("staged"), b"staged");
    }

    #[test]
    fn delete_unknown_commit_neither_rolls_back_nor_cleans_up() {
        let (_temp, vault) = temp_vault();
        ensure_layout(&vault).expect("layout");
        let id = hex_encode(&[0x52; 16]);
        let original = ciphertext_path(&vault, &id);
        let staged = delete_staging_path(&vault, &id);
        fs::create_dir_all(original.parent().expect("parent")).expect("parent");
        fs::write(&staged, b"staged").expect("staged");

        let err = apply_delete_index_outcome(
            Err(IndexSaveError::unknown(VaultError::disk())),
            &staged,
            &original,
        )
        .expect_err("unknown must stop");

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert!(!original.exists());
        assert_eq!(fs::read(&staged).expect("staged"), b"staged");
    }

    #[cfg(unix)]
    #[test]
    fn committed_delete_unlink_failure_returns_cleanup_pending() {
        use std::os::unix::fs::PermissionsExt;

        let (_temp, vault) = temp_vault();
        ensure_layout(&vault).expect("layout");
        let staged = delete_staging_path(&vault, &hex_encode(&[0x53; 16]));
        fs::write(&staged, b"staged").expect("staged");
        let tmp = vault.join(TMP_DIR);
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o500)).expect("chmod");

        let pending = cleanup_committed_delete(&staged).expect("cleanup result");
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).expect("chmod back");

        assert!(pending);
        assert!(staged.is_file());
    }

    #[test]
    fn recovery_s0_and_s3_are_noops() {
        let (_temp, vault, key, entries) = delete_fixture(&["states.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);

        prepare_vault(&vault, &key).expect("S0");
        assert!(original.is_file());
        assert!(!staged.exists());

        let mut index = Index::load(&vault, &key).expect("load");
        index.entries.clear();
        index.save(&vault, &key).expect("save empty index");
        fs::remove_file(&original).expect("remove original");
        prepare_vault(&vault, &key).expect("S3");
        assert!(!original.exists());
        assert!(!staged.exists());
        assert!(Index::load(&vault, &key).expect("load").entries.is_empty());
    }

    #[test]
    fn recovery_s1_restores_indexed_ciphertext() {
        let (_temp, vault, key, entries) = delete_fixture(&["s1.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);
        let before = fs::read(&original).expect("original");
        fs::rename(&original, &staged).expect("make S1");

        prepare_vault(&vault, &key).expect("recover S1");

        assert_eq!(fs::read(&original).expect("restored"), before);
        assert!(!staged.exists());
        assert_eq!(Index::load(&vault, &key).expect("load").entries.len(), 1);
    }

    #[test]
    fn recovery_s2_cleans_only_committed_staging() {
        let (_temp, vault, key, entries) = delete_fixture(&["s2.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);
        fs::rename(&original, &staged).expect("make S2");
        let mut index = Index::load(&vault, &key).expect("load");
        index.entries.clear();
        index.save(&vault, &key).expect("commit empty index");

        prepare_vault(&vault, &key).expect("recover S2");

        assert!(!original.exists());
        assert!(!staged.exists());
        assert!(Index::load(&vault, &key).expect("load").entries.is_empty());
    }

    #[test]
    fn recovery_refuses_original_and_staging_together() {
        let (_temp, vault, key, entries) = delete_fixture(&["both.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);
        fs::copy(&original, &staged).expect("make ambiguous state");
        let original_before = fs::read(&original).expect("original");
        let staged_before = fs::read(&staged).expect("staged");

        let err = prepare_vault(&vault, &key).expect_err("must stop");

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert_eq!(
            fs::read(&original).expect("original after"),
            original_before
        );
        assert_eq!(fs::read(&staged).expect("staged after"), staged_before);
    }

    #[test]
    fn recovery_missing_index_does_not_write_or_clean_staging() {
        let (_temp, vault, key, entries) = delete_fixture(&["no-index.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);
        fs::rename(&original, &staged).expect("make S1");
        fs::remove_file(vault.join(INDEX_FILE)).expect("remove index");
        let before = fs::read(&staged).expect("staged");

        let err = prepare_vault(&vault, &key).expect_err("missing index");

        assert_eq!(err.code, "INDEX_MISSING");
        assert_eq!(fs::read(&staged).expect("staged after"), before);
        assert!(!original.exists());
        assert!(!vault.join(INDEX_FILE).exists());
    }

    #[test]
    fn recovery_corrupt_index_or_wrong_key_does_not_write() {
        for wrong_key in [false, true] {
            let (_temp, vault, key, entries) = delete_fixture(&["auth.txt"]);
            let id = &entries[0].id;
            let original = ciphertext_path(&vault, id);
            let staged = delete_staging_path(&vault, id);
            fs::rename(&original, &staged).expect("make S1");
            if !wrong_key {
                let index_path = vault.join(INDEX_FILE);
                let mut raw = fs::read(&index_path).expect("index");
                let last = raw.len() - 1;
                raw[last] ^= 1;
                fs::write(&index_path, raw).expect("corrupt index");
            }
            let staged_before = fs::read(&staged).expect("staged");
            let index_before = fs::read(vault.join(INDEX_FILE)).expect("index before");
            let other = Zeroizing::new([0xA5; 32]);
            let attempt_key = if wrong_key { &other } else { &key };

            let err = prepare_vault(&vault, attempt_key).expect_err("authentication");

            assert_eq!(err.code, "INDEX_INVALID");
            assert_eq!(fs::read(&staged).expect("staged after"), staged_before);
            assert_eq!(
                fs::read(vault.join(INDEX_FILE)).expect("index after"),
                index_before
            );
            assert!(!original.exists());
        }
    }

    #[test]
    fn invalid_delete_staging_name_stops_recovery() {
        let (_temp, vault, key, entries) = delete_fixture(&["strict.txt"]);
        let invalid = vault.join(TMP_DIR).join("delete-not-a-valid-id.hvn");
        fs::write(&invalid, b"preserve").expect("invalid staging");
        let original = ciphertext_path(&vault, &entries[0].id);

        let err = prepare_vault(&vault, &key).expect_err("invalid name");

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert_eq!(fs::read(&invalid).expect("invalid staging"), b"preserve");
        assert!(original.is_file());
    }

    #[test]
    fn import_auth_or_recovery_failure_preserves_app_temp() {
        for corrupt_index in [true, false] {
            let (_temp, vault, key, entries) = delete_fixture(&["preserve-temp.txt"]);
            let index_temp = vault.join(".index.tmp-preserved");
            let import_temp = vault.join(TMP_DIR).join("import-preserved.part");
            fs::write(&index_temp, b"index recovery copy").expect("index temp");
            fs::write(&import_temp, b"import recovery copy").expect("import temp");

            let expected_code = if corrupt_index {
                let index_path = vault.join(INDEX_FILE);
                let mut raw = fs::read(&index_path).expect("index");
                let last = raw.len() - 1;
                raw[last] ^= 1;
                fs::write(&index_path, raw).expect("corrupt index");
                "INDEX_INVALID"
            } else {
                let original = ciphertext_path(&vault, &entries[0].id);
                fs::copy(&original, delete_staging_path(&vault, &entries[0].id))
                    .expect("ambiguous delete copies");
                "VAULT_RECOVERY_REQUIRED"
            };

            let err = import_batch(&vault, &key, &[], &|| false, &|_| {})
                .expect_err("preflight must fail");
            assert_eq!(err.code, expected_code);
            assert_eq!(
                fs::read(&index_temp).expect("index temp after"),
                b"index recovery copy"
            );
            assert_eq!(
                fs::read(&import_temp).expect("import temp after"),
                b"import recovery copy"
            );
        }
    }

    #[test]
    fn import_cleans_app_temp_after_successful_preflight() {
        let (_temp, vault, key, _entries) = delete_fixture(&["clean-temp.txt"]);
        let index_temp = vault.join(".index.tmp-clean");
        let import_temp = vault.join(TMP_DIR).join("import-clean.part");
        fs::write(&index_temp, b"stale").expect("index temp");
        fs::write(&import_temp, b"stale").expect("import temp");

        import_batch(&vault, &key, &[], &|| false, &|_| {}).expect("import preflight");

        assert!(!index_temp.exists());
        assert!(!import_temp.exists());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_delete_staging_name_is_rejected() {
        use std::os::unix::ffi::OsStrExt;

        let err = parse_delete_staging_os_name(OsStr::from_bytes(b"delete-\xff.hvn"))
            .expect_err("invalid UTF-8 name");
        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert!(
            parse_delete_staging_os_name(OsStr::from_bytes(b"other-\xff.hvn"))
                .expect("unrelated name")
                .is_none()
        );
        let valid = format!("delete-{}.hvn", "0".repeat(32));
        assert_eq!(
            parse_delete_staging_os_name(OsStr::new(&valid)).expect("valid name"),
            Some("0".repeat(32))
        );
    }

    #[test]
    fn delete_cancellation_happens_before_quarantine() {
        let (_temp, vault, key, entries) = delete_fixture(&["cancel.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);

        let err = delete_file(&vault, &key, id, &|| true).expect_err("cancel");

        assert_eq!(err.code, "DELETE_CANCELLED");
        assert!(original.is_file());
        assert!(!staged.exists());
        assert_eq!(Index::load(&vault, &key).expect("load").entries.len(), 1);
    }

    #[test]
    fn vault_locks_enforce_shared_and_exclusive_modes() {
        let (_temp, vault) = temp_vault();
        ensure_layout(&vault).expect("layout");

        let shared_a = VaultLock::acquire_shared(&vault).expect("shared A");
        let shared_b = VaultLock::acquire_shared(&vault).expect("shared B");
        assert_eq!(
            VaultLock::acquire(&vault)
                .err()
                .expect("exclusive conflict")
                .code,
            "VAULT_BUSY"
        );
        drop(shared_a);
        drop(shared_b);

        let exclusive = VaultLock::acquire(&vault).expect("exclusive");
        assert_eq!(
            VaultLock::acquire_shared(&vault)
                .err()
                .expect("shared conflict")
                .code,
            "VAULT_BUSY"
        );
        drop(exclusive);
        assert!(VaultLock::acquire_shared(&vault).is_ok());
    }

    #[test]
    fn delete_unknown_after_rename_keeps_staging_and_skips_rollback() {
        let (_temp, vault, key, entries) = delete_fixture(&["unknown-delete.txt"]);
        let id = &entries[0].id;
        let original = ciphertext_path(&vault, id);
        let staged = delete_staging_path(&vault, id);

        // 模拟：临时索引写入并同步成功、rename 成功、目录 fsync 失败。
        fault_injection::set_index_save_fsync_failure(true);
        let err = delete_file(&vault, &key, id, &|| false).expect_err("unknown");
        fault_injection::set_index_save_fsync_failure(false);

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        // 不得回滚：原路径不得重新出现。
        assert!(!original.exists());
        // 不得 unlink：暂存密文仍在。
        assert!(staged.is_file());
        // rename 已生效，磁盘索引应已不含该条目。
        assert!(Index::load(&vault, &key).expect("load").entries.is_empty());

        // 重新认证后的恢复按实际索引状态继续清理暂存密文。
        let index = Index::load(&vault, &key).expect("load for recovery");
        recover_delete_files(&vault, &index).expect("recovery");
        assert!(!staged.exists());
        assert!(!original.exists());
    }

    #[test]
    fn import_unknown_after_rename_keeps_new_ciphertext() {
        let (temp, vault, key, entries) = delete_fixture(&["existing.txt"]);
        let sources = temp.path().join("unknown-import-sources");
        fs::create_dir(&sources).expect("sources");
        let new_source = write_source(&sources, "new.txt", b"new content");

        fault_injection::set_index_save_fsync_failure(true);
        let err =
            import_batch(&vault, &key, &[new_source], &|| false, &|_| {}).expect_err("unknown");
        fault_injection::set_index_save_fsync_failure(false);

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        let index = Index::load(&vault, &key).expect("load");
        assert_eq!(index.entries.len(), 2);
        assert!(index.entries.iter().any(|entry| entry.id == entries[0].id));
        // 新密文不得因提交状态不明而被删除。
        for entry in &index.entries {
            assert!(ciphertext_path(&vault, &entry.id).is_file());
        }
    }

    #[cfg(unix)]
    #[test]
    fn cross_process_locks_conflict_and_release_on_exit() {
        use std::io::{BufRead, BufReader};
        use std::process::{Child, Command, Stdio};

        let (_temp, vault) = temp_vault();
        ensure_layout(&vault).expect("layout");
        let exe = std::env::current_exe().expect("test exe");

        fn spawn(exe: &Path, mode: &str, vault: &Path) -> Child {
            Command::new(exe)
                .args([
                    "--exact",
                    "file_store::tests::vault_lock_helper_process",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(
                    "HAVEN_LOCK_HELPER",
                    format!("{mode}\n{}", vault.to_string_lossy()),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn helper")
        }

        /// libtest 输出走 stderr；读取包含结果标记的那一行。
        fn first_line(child: &mut Child) -> String {
            let stderr = child.stderr.take().expect("stderr");
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).expect("read helper") == 0 {
                    return String::new();
                }
                let trimmed = line.trim();
                if trimmed.contains("ACQUIRED") {
                    return "ACQUIRED".to_string();
                }
                if let Some(pos) = trimmed.find("BUSY:") {
                    return trimmed[pos..].trim().to_string();
                }
            }
        }

        // 1. 本进程持共享锁时，子进程排他锁失败。
        let shared = VaultLock::acquire_shared(&vault).expect("shared");
        let mut blocked = spawn(&exe, "ex", &vault);
        assert_eq!(first_line(&mut blocked), "BUSY:VAULT_BUSY");
        blocked.wait().expect("wait blocked");
        drop(shared);

        // 2. 子进程持排他锁时，本进程共享锁失败。
        let mut holder = spawn(&exe, "ex", &vault);
        assert_eq!(first_line(&mut holder), "ACQUIRED");
        assert_eq!(
            VaultLock::acquire_shared(&vault)
                .err()
                .expect("shared conflict")
                .code,
            "VAULT_BUSY"
        );

        // 3. 两个进程同时竞争排他锁，只有一个成功。
        let mut loser = spawn(&exe, "ex", &vault);
        assert_eq!(first_line(&mut loser), "BUSY:VAULT_BUSY");
        loser.wait().expect("wait loser");

        // 4. 子进程异常退出后，内核释放锁，另一进程可获取。
        holder.kill().expect("kill holder");
        holder.wait().expect("wait killed");
        let mut after_kill = spawn(&exe, "ex", &vault);
        assert_eq!(first_line(&mut after_kill), "ACQUIRED");
        // 关闭 stdin 让子进程安全退出并释放锁。
        drop(after_kill.stdin.take());
        after_kill.wait().expect("wait after kill");
        assert!(VaultLock::acquire(&vault).is_ok());
    }

    /// 跨进程测试的子进程入口；未设置环境变量时直接通过。
    #[cfg(unix)]
    #[test]
    fn vault_lock_helper_process() {
        let Ok(spec) = std::env::var("HAVEN_LOCK_HELPER") else {
            return;
        };
        let (mode, dir) = spec.split_once('\n').expect("helper spec");
        let dir = PathBuf::from(dir);
        let acquired = if mode == "ex" {
            VaultLock::acquire(&dir)
        } else {
            VaultLock::acquire_shared(&dir)
        };
        match acquired {
            Ok(_lock) => {
                eprintln!("ACQUIRED");
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
            }
            Err(err) => eprintln!("BUSY:{}", err.code),
        }
    }

    #[test]
    fn import_unknown_commit_preserves_new_ciphertext() {
        let (_temp, vault, key, entries) = delete_fixture(&["import-unknown.txt"]);
        let mut index = Index::load(&vault, &key).expect("load");
        let final_path = ciphertext_path(&vault, &entries[0].id);
        let before = fs::read(&final_path).expect("ciphertext");

        let err = apply_import_index_outcome(
            Err(IndexSaveError::unknown(VaultError::disk())),
            &mut index,
            &final_path,
        )
        .expect_err("unknown must stop");

        assert_eq!(err.code, "VAULT_RECOVERY_REQUIRED");
        assert_eq!(fs::read(&final_path).expect("ciphertext after"), before);
        assert_eq!(index.entries.len(), 1);
    }

    #[test]
    fn delete_errors_do_not_leak_names_paths_keys_or_plaintext() {
        let (temp, vault, key, entries) = delete_fixture(&["敏感 名称.txt"]);
        let errors = [
            delete_file(&vault, &key, "bad id", &|| false).expect_err("invalid id"),
            delete_file(&vault, &key, "00000000000000000000000000000000", &|| false)
                .expect_err("unknown id"),
            VaultError::delete_path_invalid(),
            VaultError::delete_incomplete(),
            VaultError::recovery_required(),
        ];
        let secrets = [
            "敏感 名称.txt".to_string(),
            temp.path().to_string_lossy().into_owned(),
            hex_encode(&*key),
            entries[0].id.clone(),
        ];

        for error in errors {
            let rendered = format!("{:?} {} {}", error, error.code, error.message);
            for secret in &secrets {
                assert!(!rendered.contains(secret));
            }
        }
    }
}
