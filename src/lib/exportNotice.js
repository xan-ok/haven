/**
 * 导出成功提示的统一样式。
 *
 * 导出的文件是普通明文，离开 Haven 后不再受加密保护，因此三种导出路径
 * （单文件、批量、文件夹）必须给出同等语义的警告。
 */
export const EXPORT_PLAINTEXT_NOTICE =
  "导出文件是未加密副本，已不再受 Haven 保护，请妥善保管。";

/** 单文件导出成功提示。 */
export function singleExportSuccess(name) {
  return `导出成功：「${name}」。${EXPORT_PLAINTEXT_NOTICE}`;
}

/** 批量导出或文件夹导出成功提示。 */
export function batchExportSuccess(label, count) {
  return `${label}完成：已导出 ${count} 个文件。${EXPORT_PLAINTEXT_NOTICE}`;
}

/**
 * 批量导出或文件夹导出部分成功的提示。
 *
 * 只要实际导出了文件（`ok > 0`），就必须提示这些副本已不受保护；
 * 全部失败时不附加该警告。
 */
export function batchExportPartial(label, ok, failed) {
  const summary = `${label}完成：成功 ${ok} 个，失败 ${failed} 个。`;
  return ok > 0 ? `${summary}${EXPORT_PLAINTEXT_NOTICE}` : summary;
}

/**
 * 单文件“目录持久化未确认”提示。
 *
 * 保留原有耐久性告警，同时说明导出文件已是未加密副本；
 * 不把该状态描述成失败或确认成功。
 */
export function exportDurabilityWarning(warning) {
  return `${warning}${EXPORT_PLAINTEXT_NOTICE}`;
}
