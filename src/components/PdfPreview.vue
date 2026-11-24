<script setup>
import { onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { NButton } from "naive-ui";
import * as pdfjsLib from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.js?url";

// Worker 与 CMap 资源都随应用本地打包，运行期不发起外部网络请求。
pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

const props = defineProps({
  bytes: {
    type: Object,
    required: true,
  },
});

const MIN_SCALE = 0.5;
const MAX_SCALE = 2.5;
const SCALE_STEP = 0.25;
const MAX_CANVAS_PIXELS = 16_000_000;

const canvasRef = ref(null);
const pageNumber = ref(1);
const pageCount = ref(0);
const scale = ref(1);
const loading = ref(true);
const error = ref("");
const doc = shallowRef(null);

let renderTask = null;
let loadTicket = 0;
let disposed = false;

const percent = () => `${Math.round(scale.value * 100)}%`;

function assetBase() {
  return new URL("pdfjs/", document.baseURI).href;
}

function messageFor(source) {
  if (source?.name === "PasswordException") {
    return "该 PDF 受密码保护，暂不支持预览。";
  }
  if (source?.name === "InvalidPDFException") {
    return "无法打开该 PDF，文件可能已损坏或格式不正确。";
  }
  if (source?.name === "MissingPDFException") {
    return "PDF 内容缺失，无法预览。";
  }
  return "PDF 渲染失败，请稍后重试，或导出后使用系统阅读器查看。";
}

async function teardown() {
  if (renderTask) {
    try {
      renderTask.cancel();
    } catch {
      // 已结束的渲染任务取消失败可以忽略。
    }
    renderTask = null;
  }
  const pdf = doc.value;
  doc.value = null;
  if (pdf) {
    try {
      await pdf.destroy();
    } catch {
      // 销毁失败不影响后续内存释放。
    }
  }
  const canvas = canvasRef.value;
  if (canvas) {
    canvas.width = 0;
    canvas.height = 0;
  }
}

async function renderPage() {
  const pdf = doc.value;
  const canvas = canvasRef.value;
  if (!pdf || !canvas) return;

  const ticket = loadTicket;
  const page = await pdf.getPage(pageNumber.value);
  if (disposed || ticket !== loadTicket) return;

  const viewport = page.getViewport({ scale: scale.value });
  let ratio = Math.min(window.devicePixelRatio || 1, 2);
  if (viewport.width * viewport.height * ratio * ratio > MAX_CANVAS_PIXELS) {
    ratio = Math.max(1, Math.sqrt(MAX_CANVAS_PIXELS / (viewport.width * viewport.height)));
  }

  canvas.width = Math.floor(viewport.width * ratio);
  canvas.height = Math.floor(viewport.height * ratio);
  canvas.style.width = `${Math.floor(viewport.width)}px`;
  canvas.style.height = `${Math.floor(viewport.height)}px`;

  const context = canvas.getContext("2d");
  if (!context) throw new Error("canvas unavailable");
  context.clearRect(0, 0, canvas.width, canvas.height);

  if (renderTask) {
    try {
      renderTask.cancel();
    } catch {
      // 同上的取消容错。
    }
  }
  renderTask = page.render({
    canvasContext: context,
    viewport,
    transform: ratio === 1 ? null : [ratio, 0, 0, ratio, 0, 0],
  });
  try {
    await renderTask.promise;
  } catch (source) {
    // 翻页或缩放触发的取消不是错误。
    if (source?.name !== "RenderingCancelledException" && !disposed) {
      error.value = messageFor(source);
    }
  } finally {
    renderTask = null;
  }
}

async function loadDocument() {
  const ticket = ++loadTicket;
  loading.value = true;
  error.value = "";
  try {
    // 复制一份字节：pdf.js 会把缓冲区转移给 Worker，保留原引用以便重新挂载。
    const data = new Uint8Array(props.bytes);
    const pdf = await pdfjsLib
      .getDocument({
        data,
        cMapUrl: `${assetBase()}cmaps/`,
        cMapPacked: true,
        standardFontDataUrl: `${assetBase()}standard_fonts/`,
        isEvalSupported: false,
      })
      .promise;
    if (disposed || ticket !== loadTicket) {
      await pdf.destroy();
      return;
    }
    doc.value = pdf;
    pageCount.value = pdf.numPages;
    pageNumber.value = 1;
    await renderPage();
  } catch (source) {
    if (disposed || ticket !== loadTicket) return;
    error.value = messageFor(source);
  } finally {
    if (!disposed && ticket === loadTicket) loading.value = false;
  }
}

function goToPage(next) {
  if (!doc.value || loading.value) return;
  const target = Math.min(Math.max(1, next), pageCount.value);
  if (target === pageNumber.value) return;
  pageNumber.value = target;
  renderPage().catch((source) => {
    error.value = messageFor(source);
  });
}

function changeScale(delta) {
  if (!doc.value) return;
  const next = Math.min(Math.max(MIN_SCALE, scale.value + delta), MAX_SCALE);
  if (Math.abs(next - scale.value) < 0.001) return;
  scale.value = next;
  renderPage().catch((source) => {
    error.value = messageFor(source);
  });
}

function resetScale() {
  if (!doc.value || scale.value === 1) return;
  scale.value = 1;
  renderPage().catch((source) => {
    error.value = messageFor(source);
  });
}

onMounted(loadDocument);

watch(
  () => props.bytes,
  async () => {
    await teardown();
    if (!disposed) await loadDocument();
  }
);

onBeforeUnmount(() => {
  disposed = true;
  loadTicket += 1;
  teardown();
});
</script>

<template>
  <div class="pdf-preview">
    <div class="pdf-preview__bar">
      <n-button
        size="tiny"
        quaternary
        :disabled="loading || pageNumber <= 1"
        aria-label="上一页"
        @click="goToPage(pageNumber - 1)"
      >
        上一页
      </n-button>
      <span class="pdf-preview__page" aria-live="polite">
        {{ pageCount > 0 ? `${pageNumber} / ${pageCount}` : "—" }}
      </span>
      <n-button
        size="tiny"
        quaternary
        :disabled="loading || pageNumber >= pageCount"
        aria-label="下一页"
        @click="goToPage(pageNumber + 1)"
      >
        下一页
      </n-button>
      <span class="pdf-preview__divider" aria-hidden="true"></span>
      <n-button size="tiny" quaternary :disabled="loading || scale <= MIN_SCALE" aria-label="缩小" @click="changeScale(-SCALE_STEP)">
        −
      </n-button>
      <n-button size="tiny" quaternary :disabled="loading" :aria-label="`当前缩放 ${percent()}`" @click="resetScale">
        {{ percent() }}
      </n-button>
      <n-button size="tiny" quaternary :disabled="loading || scale >= MAX_SCALE" aria-label="放大" @click="changeScale(SCALE_STEP)">
        ＋
      </n-button>
    </div>

    <div class="pdf-preview__stage">
      <p v-if="loading" class="haven-preview__hint">正在渲染 PDF…</p>
      <p v-else-if="error" class="haven-preview__hint haven-preview__hint--error" role="alert">
        {{ error }}
      </p>
      <canvas
        v-show="!error"
        ref="canvasRef"
        class="pdf-preview__canvas"
        role="img"
        :aria-label="`PDF 第 ${pageNumber} 页，共 ${pageCount} 页`"
      ></canvas>
    </div>
  </div>
</template>
