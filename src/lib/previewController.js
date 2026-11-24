// 图片预览状态机：内存字节 -> Blob URL 的生命周期管理。
//
// 安全边界：
// - 这里只接收 Rust 后端解密后的原始字节，永远看不到主密钥或子密钥。
// - 每次打开/关闭都会递增请求序号，旧请求的结果不会覆盖新请求。
// - Blob URL 在关闭、切换、锁定、组件卸载时都会撤销。

// 与 Rust 侧 `PREVIEW_MAX_BYTES` 保持一致；后端是最终裁判，这里只做提前提示。
export const PREVIEW_MAX_BYTES = 20 * 1024 * 1024;

const IDLE_STATE = Object.freeze({
  status: "idle",
  id: null,
  name: null,
  url: null,
  message: "",
  code: null,
});

export function previewTypeFor(name) {
  if (typeof name !== "string") return null;
  const lower = name.toLowerCase();
  if (lower.endsWith(".jpg") || lower.endsWith(".jpeg")) {
    return { mime: "image/jpeg" };
  }
  if (lower.endsWith(".png")) {
    return { mime: "image/png" };
  }
  return null;
}

function toBytes(raw) {
  if (raw instanceof ArrayBuffer) return new Uint8Array(raw);
  if (raw instanceof Uint8Array) return raw;
  if (Array.isArray(raw)) return Uint8Array.from(raw);
  return null;
}

function errorText(error) {
  if (typeof error?.message === "string" && error.message.length > 0) {
    return error.message;
  }
  return "图片解密失败，请稍后重试。";
}

export function createPreviewController({
  loadBytes,
  createObjectUrl,
  revokeObjectUrl,
  notify,
}) {
  let state = { ...IDLE_STATE };
  let disposed = false;
  let requestId = 0;

  function publish(next) {
    state = next;
    notify?.({ ...state });
  }

  function release(url) {
    if (url) revokeObjectUrl(url);
  }

  function fail(file, code, message) {
    publish({
      status: "error",
      id: file?.id ?? null,
      name: file?.name ?? null,
      url: null,
      message,
      code,
    });
  }

  function close() {
    requestId += 1;
    release(state.url);
    publish({ ...IDLE_STATE });
  }

  async function open(file) {
    const type = previewTypeFor(file?.name);
    if (!type) {
      close();
      fail(file, "PREVIEW_UNSUPPORTED", "该文件类型暂不支持预览。");
      return;
    }
    const size = Number(file?.size);
    if (Number.isFinite(size) && size > PREVIEW_MAX_BYTES) {
      close();
      fail(file, "PREVIEW_TOO_LARGE", "图片超过 20 MB 的预览上限，暂不支持预览。");
      return;
    }

    const ticket = ++requestId;
    // 切换图片时立即释放上一张的 URL，避免旧明文留在内存中。
    release(state.url);
    publish({
      status: "loading",
      id: file.id,
      name: file.name,
      url: null,
      message: "",
      code: null,
    });

    try {
      const bytes = toBytes(await loadBytes(file.id));
      if (disposed || ticket !== requestId) return;
      if (!bytes || bytes.byteLength === 0) {
        const empty = new Error("图片内容为空。");
        empty.code = "PREVIEW_INVALID";
        throw empty;
      }
      const blob = new Blob([bytes], { type: type.mime });
      const url = createObjectUrl(blob);
      if (disposed || ticket !== requestId) {
        // 创建期间请求已过期：立即撤销，绝不发布。
        release(url);
        return;
      }
      publish({
        status: "ready",
        id: file.id,
        name: file.name,
        url,
        message: "",
        code: null,
      });
    } catch (error) {
      if (disposed || ticket !== requestId) return;
      fail(
        file,
        typeof error?.code === "string" ? error.code : "PREVIEW_FAILED",
        errorText(error)
      );
    }
  }

  function dispose() {
    disposed = true;
    requestId += 1;
    release(state.url);
    state = { ...IDLE_STATE };
  }

  return {
    open,
    close,
    dispose,
    getState: () => ({ ...state }),
  };
}
