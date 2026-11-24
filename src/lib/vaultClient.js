// 前端与 Rust 保险库命令的唯一通道。
// 前端只接收状态、路径与错误信息，永远拿不到主密钥或密码派生结果。

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open, save } from "@tauri-apps/plugin-dialog";

export const IMPORT_PROGRESS_EVENT = "haven://import-progress";
export const BACKUP_PROGRESS_EVENT = "haven://backup-progress";
export const RESTORE_PROGRESS_EVENT = "haven://restore-progress";

/** 是否运行在 Tauri 桌面壳中。浏览器预览时不得伪造保险库能力。 */
export function isTauri() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

// 后端一旦报告保险库不可用，说明会话主密钥已被清空；界面通过该回调统一收尾。
let vaultUnavailableHandler = null;

/** 注册保险库不可用的统一回调；返回取消注册函数。 */
export function onVaultUnavailable(handler) {
  vaultUnavailableHandler = handler;
  return () => {
    if (vaultUnavailableHandler === handler) {
      vaultUnavailableHandler = null;
    }
  };
}

/**
 * 所有保险库命令的统一入口：成功原样返回，失败原样抛出，
 * 仅在 `VAULT_UNAVAILABLE` 时额外通知界面做失败关闭收尾。
 */
function vaultCommand(name, args) {
  return invoke(name, args).catch((error) => {
    if (normalizeError(error).code === "VAULT_UNAVAILABLE") {
      vaultUnavailableHandler?.();
    }
    throw error;
  });
}

export function getVaultStatus() {
  return vaultCommand("vault_status");
}

export function createVault(parentDir, password) {
  return vaultCommand("create_vault", { parentDir, password });
}

export function openVault(dir) {
  return vaultCommand("open_vault", { dir });
}

export function unlockVault(password) {
  return vaultCommand("unlock_vault", { password });
}

export function lockVault() {
  return vaultCommand("lock_vault");
}

/** 系统目录选择器；取消时返回 null。 */
export async function chooseDirectory(title) {
  const selected = await open({ directory: true, multiple: false, title });
  return typeof selected === "string" ? selected : null;
}

/** FR-38：系统目录选择器，支持多选；取消时返回空数组。 */
export async function chooseDirectories(title) {
  const selected = await open({ directory: true, multiple: true, title });
  if (Array.isArray(selected)) {
    return selected.filter((path) => typeof path === "string");
  }
  return typeof selected === "string" ? [selected] : [];
}

/** 系统文件选择器；取消时返回空数组。 */
export async function chooseFiles(title) {
  const selected = await open({ directory: false, multiple: true, title });
  if (Array.isArray(selected)) {
    return selected.filter((path) => typeof path === "string");
  }
  return typeof selected === "string" ? [selected] : [];
}

export function importFiles(paths) {
  return vaultCommand("import_files", { paths });
}

export function cancelImport() {
  return vaultCommand("cancel_import");
}

export function listFiles() {
  return vaultCommand("list_files");
}

export function verifyFiles() {
  return vaultCommand("verify_files");
}

/** 请求解密一张图片用于预览；返回 ArrayBuffer（原始字节，非 JSON）。 */
export function previewImage(id) {
  return vaultCommand("preview_image", { id });
}

/** FR-39：由 Rust 解密、缩放并返回 PNG 缩略图字节。 */
export function previewThumbnail(id) {
  return vaultCommand("preview_thumbnail", { id });
}

/** FR-40：由 Rust 解密并校验后返回视频字节，前端仅创建内存 Blob。 */
export function previewVideo(id) {
  return vaultCommand("preview_video", { id });
}

/** FR-34：在内存中解密 PDF，返回原始字节；明文不落盘。 */
export function previewPdf(id) {
  return vaultCommand("preview_pdf", { id });
}

/** 系统保存对话框；取消时返回 null。 */
export async function chooseSavePath(defaultName, title) {
  const selected = await save({ title, defaultPath: defaultName });
  return typeof selected === "string" ? selected : null;
}

/** 请求后端把文件解密导出到目标路径；返回 { name, size }。 */
export function exportFile(id, targetPath) {
  return vaultCommand("export_file", { id, targetPath });
}

/** 删除索引条目及其库内密文；返回 { cleanupPending }。 */
export function deleteFile(id) {
  return vaultCommand("delete_file", { id });
}

/** FR-32：只修改索引中的显示名称；返回更新后的 { id, name, size, importedAt }。 */
export function renameFile(id, name) {
  return vaultCommand("rename_file", { id, name });
}

/** FR-41：收藏状态写入加密索引，返回更新后的文件条目。 */
export function setFavorite(id, favorite) {
  return vaultCommand("set_favorite", { id, favorite });
}

/** FR-42：读取一级文件夹列表（加密索引内）。 */
export function listFolders() {
  return vaultCommand("list_folders");
}

/** FR-42：创建文件夹，返回 { id, name }。 */
export function createFolder(name) {
  return vaultCommand("create_folder", { name });
}

/** FR-42：重命名文件夹，返回 { id, name }。 */
export function renameFolder(id, name) {
  return vaultCommand("rename_folder", { id, name });
}

/** FR-42：删除空文件夹。 */
export function deleteFolder(id) {
  return vaultCommand("delete_folder", { id });
}

/** FR-42：批量移动文件；folderId 为 null 表示移回根目录。 */
export function moveFiles(ids, folderId) {
  return vaultCommand("move_files", { ids, folderId });
}

/** FR-44：备份到所选目录，返回 { destination, fileCount, totalBytes }。 */
export function backupVault(targetDir) {
  return vaultCommand("backup_vault", { targetDir });
}

/** 订阅后端备份进度；载荷只含已处理数量与字节数，不含文件名。 */
export async function onBackupProgress(handler) {
  if (!isTauri()) return () => {};
  return listen(BACKUP_PROGRESS_EVENT, (event) => handler(event.payload));
}

/** FR-46：验证备份目录对应的主密码；只读校验，不创建任何目录。 */
export function verifyBackupPassword(backupDir, password) {
  return vaultCommand("verify_backup_password", { backupDir, password });
}

/** FR-45/46：用备份自身的主密码恢复到所选目录，返回 { destination, fileCount, totalBytes }。 */
export function restoreVault(backupDir, targetDir, password) {
  return vaultCommand("restore_vault", { backupDir, targetDir, password });
}

/** 订阅后端恢复进度；载荷只含已处理数量与字节数，不含文件名。 */
export async function onRestoreProgress(handler) {
  if (!isTauri()) return () => {};
  return listen(RESTORE_PROGRESS_EVENT, (event) => handler(event.payload));
}

/** 订阅后端发送的真实导入进度；返回取消订阅函数。 */
export async function onImportProgress(handler) {
  if (!isTauri()) return () => {};
  return listen(IMPORT_PROGRESS_EVENT, (event) => handler(event.payload));
}

/**
 * FR-35：订阅 Tauri 的文件拖拽事件，回调解收到 `enter/over/leave/drop` 载荷。
 * 浏览器预览或非 Tauri 环境没有拖拽能力，返回空订阅函数。
 */
export async function onFileDrop(handler) {
  if (!isTauri()) return () => {};
  return getCurrentWebviewWindow().onDragDropEvent((event) => handler(event.payload));
}

/** 把后端返回的结构化错误整理成 { code, message }。 */
export function normalizeError(error) {
  if (error && typeof error === "object" && typeof error.code === "string") {
    return {
      code: error.code,
      message: typeof error.message === "string" ? error.message : "操作失败，请重试。",
    };
  }
  return { code: "INTERNAL", message: "操作失败，请重试。" };
}
