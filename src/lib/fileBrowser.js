// 文件浏览：搜索、类型筛选与排序，全部在当前页面内存中完成。
// 不接触密钥、不解密、不写入任何持久化状态。

import { fileTypeCategory } from "./fileFormat.js";

export const FILE_TYPE_ALL = "all";

export const DEFAULT_QUERY = Object.freeze({
  keyword: "",
  type: FILE_TYPE_ALL,
  sortKey: "importedAt",
  sortDirection: "desc",
  favoriteOnly: false,
  // FR-42：null 表示全部文件（跨文件夹），字符串表示指定文件夹 ID。
  folderId: null,
});

const SORT_KEYS = new Set(["importedAt", "size"]);
const PAGE_SIZE = 60;

function toNumber(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number : 0;
}

/** 搜索关键词去除首尾空格；非字符串按空关键词处理。 */
export function normalizeKeyword(keyword) {
  return typeof keyword === "string" ? keyword.trim() : "";
}

/** 文件名是否匹配关键词：大小写不敏感，支持中文；空关键词匹配全部。 */
export function matchesKeyword(name, keyword) {
  const needle = normalizeKeyword(keyword).toLowerCase();
  if (!needle) return true;
  return typeof name === "string" && name.toLowerCase().includes(needle);
}

/** 类型筛选；分类规则统一来自 fileFormat.js，避免两套扩展名判断。 */
export function matchesType(name, type) {
  if (!type || type === FILE_TYPE_ALL) return true;
  return fileTypeCategory(name) === type;
}

/** 收藏筛选：仅在收藏位为 true 时保留。 */
export function matchesFavorite(file, favoriteOnly) {
  return favoriteOnly !== true || file?.favorite === true;
}

/** 文件夹筛选：null/undefined 表示全部文件，否则只保留归属该文件夹的文件。 */
export function matchesFolder(file, folderId) {
  if (folderId === null || folderId === undefined) return true;
  return file?.folderId === folderId;
}

/** 统计每个文件夹中的文件数量；根目录文件不计入任何文件夹。 */
export function countFilesByFolder(files) {
  const counts = {};
  for (const file of Array.isArray(files) ? files : []) {
    const folderId = file?.folderId;
    if (typeof folderId !== "string" || folderId.length === 0) continue;
    counts[folderId] = (counts[folderId] || 0) + 1;
  }
  return counts;
}

/** 返回筛选后的新数组，不修改输入。 */
export function filterFiles(
  files,
  { keyword = "", type = FILE_TYPE_ALL, favoriteOnly = false, folderId = null } = {}
) {
  if (!Array.isArray(files)) return [];
  const trimmed = normalizeKeyword(keyword);
  const onlyFavorites = favoriteOnly === true;
  const scopedFolder = typeof folderId === "string" && folderId.length > 0 ? folderId : null;
  if (!trimmed && (!type || type === FILE_TYPE_ALL) && !onlyFavorites && !scopedFolder) {
    return [...files];
  }
  return files.filter(
    (file) =>
      matchesKeyword(file?.name, trimmed) &&
      matchesType(file?.name, type) &&
      matchesFavorite(file, onlyFavorites) &&
      matchesFolder(file, scopedFolder)
  );
}

/**
 * 返回排序后的新数组，不修改输入。
 * 主排序值相同时，先按导入时间倒序，再按文件名、id 升序兜底，保证结果稳定。
 */
export function sortFiles(files, { key = "importedAt", direction = "desc" } = {}) {
  if (!Array.isArray(files)) return [];
  const sortKey = SORT_KEYS.has(key) ? key : "importedAt";
  const factor = direction === "asc" ? 1 : -1;
  const valueOf = (file) => (sortKey === "size" ? toNumber(file?.size) : toNumber(file?.importedAt));

  return [...files].sort((left, right) => {
    const primary = (valueOf(left) - valueOf(right)) * factor;
    if (primary !== 0) return primary;

    const byTime = toNumber(right?.importedAt) - toNumber(left?.importedAt);
    if (byTime !== 0) return byTime;

    const nameLeft = String(left?.name ?? "");
    const nameRight = String(right?.name ?? "");
    if (nameLeft !== nameRight) return nameLeft < nameRight ? -1 : 1;
    return String(left?.id ?? "").localeCompare(String(right?.id ?? ""));
  });
}

/** 先筛选后排序，返回新的匹配结果数组。 */
export function applyQuery(files, query = {}) {
  return sortFiles(filterFiles(files, query), {
    key: query.sortKey,
    direction: query.sortDirection,
  });
}

/**
 * 文件浏览状态机：负责分页重置与派生结果，供 VaultPanel 订阅。
 * 任何查询条件变化或文件列表刷新都会把显示数量重置为 pageSize。
 */
export function createFileBrowser({ pageSize = PAGE_SIZE, onChange } = {}) {
  const size = Math.max(1, Math.floor(Number(pageSize)) || PAGE_SIZE);
  let files = [];
  let query = { ...DEFAULT_QUERY };
  let visibleCount = size;

  function snapshot() {
    const matched = applyQuery(files, query);
    return {
      total: files.length,
      matched,
      visible: matched.slice(0, visibleCount),
      hiddenCount: Math.max(0, matched.length - visibleCount),
      visibleCount,
      query: { ...query },
      isFiltering:
        normalizeKeyword(query.keyword) !== "" ||
        query.type !== FILE_TYPE_ALL ||
        query.favoriteOnly === true,
    };
  }

  function publish() {
    onChange?.(snapshot());
  }

  return {
    snapshot,
    setFiles(next) {
      files = Array.isArray(next) ? next : [];
      visibleCount = size;
      publish();
    },
    setQuery(patch = {}) {
      if (!patch || typeof patch !== "object") return;
      query = { ...query, ...patch };
      visibleCount = size;
      publish();
    },
    clearFilters() {
      query = { ...query, keyword: "", type: FILE_TYPE_ALL, favoriteOnly: false };
      visibleCount = size;
      publish();
    },
    showMore() {
      const matchedLength = applyQuery(files, query).length;
      if (visibleCount >= matchedLength) return;
      visibleCount += size;
      publish();
    },
    reset() {
      files = [];
      query = { ...DEFAULT_QUERY };
      visibleCount = size;
      publish();
    },
  };
}
