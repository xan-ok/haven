// FR-39 网格缩略图：串行/限量加载 Rust 生成的缩略图，并管理 Blob URL 生命周期。
//
// 安全边界：
// - 前端只接收 Rust 端解密并缩放后的 PNG 字节，原图明文不出后端。
// - 缩略图仅以内存 Blob URL 存在，不写入磁盘；释放时立即 revoke。

const EMPTY_STATE = Object.freeze({});

export function createThumbnailController({
  loadBytes,
  createObjectUrl,
  revokeObjectUrl,
  notify,
  isImage,
  concurrency = 2,
} = {}) {
  let state = { ...EMPTY_STATE };
  const urls = new Map();
  const queue = [];
  let active = 0;
  let generation = 0;
  let disposed = false;
  const limit = Math.max(1, Number(concurrency) || 1);

  function publish() {
    notify?.({ ...state });
  }

  function set(id, value) {
    state = { ...state, [id]: value };
  }

  function request(file) {
    if (disposed || !file || typeof file.id !== "string" || !file.id) return;
    if (typeof isImage === "function" && !isImage(file.name)) return;
    // loading / ready / failed 都视为已有结果，避免重复请求。
    if (state[file.id]) return;

    set(file.id, { status: "loading", url: null });
    publish();
    queue.push({ id: file.id, generation });
    pump();
  }

  async function run(job) {
    try {
      const bytes = await loadBytes(job.id);
      if (!isCurrent(job)) return;
      const url = createObjectUrl(new Blob([bytes], { type: "image/png" }));
      urls.set(job.id, url);
      set(job.id, { status: "ready", url });
      publish();
    } catch {
      if (!isCurrent(job)) return;
      set(job.id, { status: "failed", url: null });
      publish();
    } finally {
      active -= 1;
      pump();
    }
  }

  function isCurrent(job) {
    if (disposed || job.generation !== generation) return false;
    const entry = state[job.id];
    return Boolean(entry && entry.status === "loading");
  }

  function pump() {
    while (!disposed && active < limit && queue.length > 0) {
      const job = queue.shift();
      if (!isCurrent(job)) continue;
      active += 1;
      void run(job);
    }
  }

  /** 删除单个文件的缩略图；后续晚到的结果会被丢弃。 */
  function release(id) {
    const url = urls.get(id);
    if (url) {
      revokeObjectUrl?.(url);
      urls.delete(id);
    }
    if (!(id in state)) return;
    const next = { ...state };
    delete next[id];
    state = next;
    publish();
  }

  /** 切换视图、锁定或卸载时释放全部缩略图与排队任务。 */
  function releaseAll() {
    generation += 1;
    queue.length = 0;
    for (const url of urls.values()) revokeObjectUrl?.(url);
    urls.clear();
    if (Object.keys(state).length === 0) return;
    state = {};
    publish();
  }

  function dispose() {
    disposed = true;
    generation += 1;
    queue.length = 0;
    for (const url of urls.values()) revokeObjectUrl?.(url);
    urls.clear();
    state = {};
  }

  return {
    request,
    release,
    releaseAll,
    dispose,
    getState: () => ({ ...state }),
  };
}
