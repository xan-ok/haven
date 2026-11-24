// 视频预览状态机：认证后的内存字节 -> Blob URL 生命周期管理。
//
// 安全边界：
// - 只接收 Rust 后端解密并校验后的字节，前端不接触密钥。
// - 明文视频只以内存 Blob 存在，不写入磁盘临时文件。
// - 每次打开/关闭递增请求序号，旧请求结果不会覆盖新请求。
// - Blob URL 在切换、关闭、锁定与组件卸载时立即撤销。

// 与 Rust 侧 `VIDEO_PREVIEW_MAX_BYTES` 保持一致；后端是最终裁判。
export const VIDEO_PREVIEW_MAX_BYTES = 64 * 1024 * 1024;

const IDLE_STATE = Object.freeze({
  status: "idle",
  id: null,
  name: null,
  url: null,
  message: "",
  code: null,
});

/** 常见视频扩展名到播放 MIME 的映射；仅用于展示与浏览器解码提示。 */
export function videoTypeFor(name) {
  if (typeof name !== "string") return null;
  const lower = name.toLowerCase();
  if (lower.endsWith(".mp4")) return { mime: "video/mp4" };
  if (lower.endsWith(".m4v")) return { mime: "video/x-m4v" };
  if (lower.endsWith(".mov")) return { mime: "video/quicktime" };
  if (lower.endsWith(".webm")) return { mime: "video/webm" };
  return null;
}

export function isVideoName(name) {
  return videoTypeFor(name) !== null;
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
  return "视频解密失败，请稍后重试。";
}

export function createVideoPreviewController({
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
    const type = videoTypeFor(file?.name);
    if (!type) {
      close();
      fail(file, "PREVIEW_UNSUPPORTED", "该视频格式暂不支持预览。");
      return;
    }
    const size = Number(file?.size);
    if (Number.isFinite(size) && size > VIDEO_PREVIEW_MAX_BYTES) {
      close();
      fail(file, "PREVIEW_TOO_LARGE", "视频超过 64 MB 的预览上限，暂不支持在线预览。");
      return;
    }

    const ticket = ++requestId;
    // 切换视频时立即释放上一个 Blob URL。
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
        const empty = new Error("视频内容为空。");
        empty.code = "PREVIEW_INVALID";
        throw empty;
      }
      const blob = new Blob([bytes], { type: type.mime });
      const url = createObjectUrl(blob);
      if (disposed || ticket !== requestId) {
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
