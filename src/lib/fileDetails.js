// 文件详情状态：只保存列表已有的元数据，不解密、不生成 URL、不持久化。

const FIELDS = ["id", "name", "size", "importedAt"];

function pick(file) {
  const picked = {};
  for (const field of FIELDS) picked[field] = file?.[field];
  return picked;
}

function sameFile(left, right) {
  if (!left || !right) return left === right;
  return FIELDS.every((field) => left[field] === right[field]);
}

/** 详情状态机：打开、关闭、列表刷新时校验目标是否仍然存在。 */
export function createFileDetails({ onChange } = {}) {
  let current = null;

  function publish() {
    onChange?.(current ? { ...current } : null);
  }

  return {
    getState() {
      return current ? { ...current } : null;
    },
    open(file) {
      if (!file || typeof file !== "object" || file.id == null) return;
      const next = pick(file);
      if (sameFile(current, next)) return;
      current = next;
      publish();
    },
    close() {
      if (!current) return;
      current = null;
      publish();
    },
    /** 列表刷新后同步：目标不存在则关闭，存在则更新为最新元数据。 */
    sync(files) {
      if (!current) return;
      const next = Array.isArray(files)
        ? files.find((file) => file?.id === current.id)
        : null;
      if (!next) {
        current = null;
        publish();
        return;
      }
      const picked = pick(next);
      if (sameFile(current, picked)) return;
      current = picked;
      publish();
    },
  };
}
