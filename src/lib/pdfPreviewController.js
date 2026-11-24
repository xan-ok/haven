// FR-34 PDF 预览状态机：只持有 Rust 解密后的内存字节。
//
// 安全边界：
// - 前端永远看不到主密钥或子密钥，也不把明文写成文件或 Blob URL。
// - 每次打开/关闭递增请求序号，旧请求的结果不会覆盖新请求。
// - 关闭、切换、锁定与卸载时清空字节引用，PDF 文档对象由渲染组件销毁。

// 与 Rust 侧 `PREVIEW_MAX_BYTES` 保持一致；后端是最终裁判，这里只做提前提示。
export const PDF_PREVIEW_MAX_BYTES = 20 * 1024 * 1024;

const IDLE_STATE = Object.freeze({
  status: "idle",
  id: null,
  name: null,
  bytes: null,
  message: "",
  code: null,
});

export function isPdfName(name) {
  return typeof name === "string" && name.toLowerCase().endsWith(".pdf");
}

/** 与 Rust 一致：PDF 文件头允许出现在前 1024 字节内。 */
export function hasPdfHeader(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0) return false;
  const window = bytes.subarray(0, Math.min(bytes.byteLength, 1024));
  for (let index = 0; index + 5 <= window.byteLength; index += 1) {
    if (
      window[index] === 0x25 &&
      window[index + 1] === 0x50 &&
      window[index + 2] === 0x44 &&
      window[index + 3] === 0x46 &&
      window[index + 4] === 0x2d
    ) {
      return true;
    }
  }
  return false;
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
  return "PDF 解密失败，请稍后重试。";
}

export function createPdfPreviewController({ loadBytes, notify } = {}) {
  let state = { ...IDLE_STATE };
  let disposed = false;
  let requestId = 0;

  function publish(next) {
    state = next;
    notify?.({ ...state });
  }

  function close() {
    requestId += 1;
    publish({ ...IDLE_STATE });
  }

  function fail(file, code, message) {
    publish({
      status: "error",
      id: file?.id ?? null,
      name: file?.name ?? null,
      bytes: null,
      message,
      code,
    });
  }

  async function open(file) {
    if (!isPdfName(file?.name)) {
      close();
      fail(file, "PREVIEW_UNSUPPORTED", "该文件类型暂不支持预览。");
      return;
    }
    const size = Number(file?.size);
    if (Number.isFinite(size) && size > PDF_PREVIEW_MAX_BYTES) {
      close();
      fail(file, "PREVIEW_TOO_LARGE", "PDF 超过 20 MB 的预览上限，暂不支持预览。");
      return;
    }

    const ticket = ++requestId;
    // 切换文件时立即丢弃上一份明文引用。
    publish({
      status: "loading",
      id: file.id,
      name: file.name,
      bytes: null,
      message: "",
      code: null,
    });

    try {
      const bytes = toBytes(await loadBytes(file.id));
      if (disposed || ticket !== requestId) return;
      if (!bytes || bytes.byteLength === 0) {
        const empty = new Error("PDF 内容为空。");
        empty.code = "PREVIEW_INVALID";
        throw empty;
      }
      if (!hasPdfHeader(bytes)) {
        const invalid = new Error("文件内容不是有效的 PDF。");
        invalid.code = "PREVIEW_INVALID";
        throw invalid;
      }
      publish({
        status: "ready",
        id: file.id,
        name: file.name,
        bytes,
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
    state = { ...IDLE_STATE };
  }

  return {
    open,
    close,
    dispose,
    getState: () => ({ ...state }),
  };
}
