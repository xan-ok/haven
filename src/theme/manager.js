// 主题运行时：内置主题 + 我的主题、即时应用、启动恢复与失败回退。
// 主题切换只修改 CSS 变量与 Naive UI 配置，不触碰保险库、密钥或文件数据。

import { computed, reactive, watch } from "vue";
import { BUILTIN_THEMES, DEFAULT_THEME_ID, isBuiltinThemeId } from "./presets.js";
import {
  buildTokens,
  createThemeOverrides,
  cssVariables,
  isDarkColor,
  isHexColor,
} from "./tokens.js";
import * as storage from "./storage.js";

const COLOR_KEYS = [
  "primary",
  "background",
  "surface",
  "text",
  "textMuted",
  "border",
  "success",
  "warning",
  "danger",
];

const builtinsById = new Map(BUILTIN_THEMES.map((theme) => [theme.id, theme]));

export const themeStore = reactive({
  activeId: DEFAULT_THEME_ID,
  userThemes: [],
  loaded: false,
  error: "",
});

function normalizeUserTheme(raw) {
  if (!raw || typeof raw.id !== "string" || typeof raw.name !== "string") return null;
  if (isBuiltinThemeId(raw.id) || !raw.colors || typeof raw.colors !== "object") return null;
  if (!raw.radius || typeof raw.radius !== "object") return null;
  if (!COLOR_KEYS.every((key) => isHexColor(raw.colors[key]))) return null;
  const radiusValues = [raw.radius.small, raw.radius.medium, raw.radius.large];
  if (!radiusValues.every((value) => Number.isFinite(Number(value)))) return null;
  return {
    ...raw,
    builtin: false,
    dark: isDarkColor(raw.colors.background),
  };
}

export function findTheme(id) {
  if (typeof id !== "string") return null;
  return builtinsById.get(id) || themeStore.userThemes.find((theme) => theme.id === id) || null;
}

export const builtinThemes = BUILTIN_THEMES;
export const userThemes = computed(() => themeStore.userThemes);
export const allThemes = computed(() => [...BUILTIN_THEMES, ...themeStore.userThemes]);
export const activeTheme = computed(() => findTheme(themeStore.activeId) || BUILTIN_THEMES[0]);
export const activeTokens = computed(() => buildTokens(activeTheme.value));
export const naiveOverrides = computed(() => createThemeOverrides(activeTokens.value));
export const isDarkTheme = computed(() => Boolean(activeTokens.value.dark));

export function previewTokens(id) {
  const theme = findTheme(id);
  return theme ? buildTokens(theme) : null;
}

function applyCssVariables(tokens) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  for (const [name, value] of Object.entries(cssVariables(tokens))) {
    root.style.setProperty(name, value);
  }
  root.dataset.themeMode = tokens.dark ? "dark" : "light";
  root.style.colorScheme = tokens.dark ? "dark" : "light";
}

watch(activeTokens, (tokens) => applyCssVariables(tokens), { immediate: true });

let initPromise = null;

async function bootstrapTheme() {
  let stored = null;
  try {
    const [activeId, rawThemes] = await Promise.all([
      storage.getActiveThemeId(),
      storage.listUserThemes(),
    ]);
    stored = activeId;
    themeStore.userThemes = rawThemes.map(normalizeUserTheme).filter(Boolean);
  } catch {
    themeStore.error = "无法读取主题配置，已使用默认主题。";
  }
  const resolved = findTheme(stored) ? stored : DEFAULT_THEME_ID;
  themeStore.activeId = resolved;
  themeStore.loaded = true;
  if (storage.isThemeStorageAvailable() && resolved !== stored) {
    storage.setActiveThemeId(resolved).catch(() => {});
  }
}

/** 启动时读取偏好与自定义主题；配置损坏时静默回退到 Haven Original。 */
export function initTheme() {
  if (!initPromise) initPromise = bootstrapTheme();
  return initPromise;
}

export async function refreshUserThemes() {
  const rawThemes = await storage.listUserThemes();
  themeStore.userThemes = rawThemes.map(normalizeUserTheme).filter(Boolean);
  if (!findTheme(themeStore.activeId)) {
    themeStore.activeId = DEFAULT_THEME_ID;
    storage.setActiveThemeId(DEFAULT_THEME_ID).catch(() => {});
  }
  return themeStore.userThemes;
}

export async function importThemeFile(path, overwrite = false) {
  const imported = await storage.importThemeFile(path, overwrite);
  await refreshUserThemes();
  const theme = findTheme(imported?.id);
  if (!theme) {
    const error = new Error("主题导入后无法读取，请检查文件是否完整。");
    error.code = "THEME_INVALID";
    throw error;
  }
  return theme;
}

export async function deleteTheme(id) {
  if (isBuiltinThemeId(id)) {
    const error = new Error("系统内置主题不能删除。");
    error.code = "THEME_BUILTIN";
    throw error;
  }
  if (themeStore.activeId === id) {
    const error = new Error("当前正在使用该主题，请先切换到其他主题再删除。");
    error.code = "THEME_ACTIVE";
    throw error;
  }
  await storage.deleteUserTheme(id);
  await refreshUserThemes();
}

/** 应用主题并持久化偏好；不重启、不影响已解锁状态与文件操作。 */
export async function applyTheme(id) {
  const theme = findTheme(id);
  if (!theme) {
    const error = new Error("主题不存在或已被删除。");
    error.code = "THEME_MISSING";
    throw error;
  }
  themeStore.activeId = id;
  themeStore.error = "";
  try {
    await storage.setActiveThemeId(id);
  } catch {
    const error = new Error("主题已应用，但偏好保存失败，重启后可能不会保留。");
    error.code = "THEME_SAVE_FAILED";
    throw error;
  }
  return theme;
}
