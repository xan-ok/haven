// 主题持久化通道。
// 主题文件与偏好只由 Rust 写入应用配置目录，前端不直接访问文件系统。

import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { isTauri } from "../lib/vaultClient.js";

export function isThemeStorageAvailable() {
  return isTauri();
}

export function normalizeThemeError(error) {
  if (error && typeof error === "object" && typeof error.message === "string") {
    return {
      code: typeof error.code === "string" ? error.code : "THEME_ERROR",
      message: error.message,
    };
  }
  if (typeof error === "string" && error) {
    return { code: "THEME_ERROR", message: error };
  }
  return { code: "THEME_ERROR", message: "主题操作失败，请稍后重试。" };
}

export async function listUserThemes() {
  if (!isThemeStorageAvailable()) return [];
  return invoke("theme_list_user");
}

export async function readUserTheme(id) {
  return invoke("theme_read_user", { id });
}

export async function getActiveThemeId() {
  if (!isThemeStorageAvailable()) return null;
  try {
    return await invoke("theme_get_active");
  } catch {
    return null;
  }
}

export async function setActiveThemeId(id) {
  if (!isThemeStorageAvailable()) return;
  await invoke("theme_set_active", { id });
}

export async function importThemeFile(path, overwrite = false) {
  return invoke("theme_import_file", { path, overwrite });
}

export async function deleteUserTheme(id) {
  await invoke("theme_delete_user", { id });
}

export async function chooseThemeFile() {
  const selected = await open({
    multiple: false,
    directory: false,
    title: "选择 Haven 主题文件",
    filters: [{ name: "Haven 主题", extensions: ["haventheme"] }],
  });
  return typeof selected === "string" ? selected : null;
}
