// 仅做展示格式化，不参与安全判断。

export function formatFileSize(bytes) {
  const value = Number(bytes);
  if (!Number.isFinite(value) || value < 0) return "—";
  if (value < 1024) return `${Math.trunc(value)} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let size = value / 1024;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size.toFixed(1)} ${units[unit]}`;
}

/**
 * 浏览分类：仅按扩展名推断，不做 MIME 嗅探，也不参与安全判断。
 * 分类规则只在本函数维护，列表标签与筛选共用。
 */
export function fileTypeCategory(name) {
  if (typeof name !== "string") return "other";
  const lower = name.toLowerCase();
  if (lower.endsWith(".jpg") || lower.endsWith(".jpeg") || lower.endsWith(".png")) {
    return "image";
  }
  if (lower.endsWith(".pdf")) return "pdf";
  if (
    lower.endsWith(".mp4") ||
    lower.endsWith(".mov") ||
    lower.endsWith(".m4v") ||
    lower.endsWith(".webm")
  ) {
    return "video";
  }
  return "other";
}

/** 列表中的类型标签，仅用于展示，安全判断在后端。 */
export function fileTypeLabel(name) {
  if (typeof name !== "string") return "文件";
  const category = fileTypeCategory(name);
  if (category === "image") {
    const lower = name.toLowerCase();
    if (lower.endsWith(".png")) return "PNG";
    if (lower.endsWith(".jpeg")) return "JPEG";
    return "JPG";
  }
  if (category === "pdf") return "PDF";
  if (category === "video") {
    const lower = name.toLowerCase();
    if (lower.endsWith(".webm")) return "WEBM";
    if (lower.endsWith(".mov")) return "MOV";
    if (lower.endsWith(".m4v")) return "M4V";
    return "MP4";
  }
  return "文件";
}

export function formatFileTime(seconds) {
  const value = Number(seconds);
  if (!Number.isFinite(value) || value <= 0) return "—";
  return new Date(value * 1000).toLocaleString("zh-CN", { hour12: false });
}
