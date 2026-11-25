<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { NButton, NCheckbox, NInput, NSelect, useMessage } from "naive-ui";
import FileDetailsDialog from "./FileDetailsDialog.vue";
import PdfPreview from "./PdfPreview.vue";
import VideoPreview from "./VideoPreview.vue";
import { fileTypeLabel, formatFileSize, formatFileTime } from "../lib/fileFormat.js";
import { createFileDetails } from "../lib/fileDetails.js";
import {
  DEFAULT_QUERY,
  FILE_TYPE_ALL,
  countFilesByFolder,
  createFileBrowser,
  normalizeKeyword,
} from "../lib/fileBrowser.js";
import { createFileListStore } from "../lib/fileListStore.js";
import { createDeleteController } from "../lib/deleteController.js";
import { createBatchDeleteController } from "../lib/batchDeleteController.js";
import { createBatchExportController, collectFolderExportItems } from "../lib/batchExportController.js";
import {
  batchExportPartial,
  batchExportSuccess,
  exportDurabilityWarning,
  singleExportSuccess,
} from "../lib/exportNotice.js";
import { createSelectionController } from "../lib/selectionController.js";
import {
  FILE_NAME_MAX_LENGTH,
  baseNameSelection,
  createRenameController,
} from "../lib/renameController.js";
import { createPreviewController, previewTypeFor } from "../lib/previewController.js";
import {
  createVideoPreviewController,
  isVideoName,
} from "../lib/videoPreviewController.js";
import { createPdfPreviewController, isPdfName } from "../lib/pdfPreviewController.js";
import { createDragDropController } from "../lib/dragDropController.js";
import { createThumbnailController } from "../lib/thumbnailController.js";
import {
  backupVault,
  cancelImport,
  chooseDirectories,
  chooseDirectory,
  chooseFiles,
  chooseSavePath,
  createFolder,
  deleteFile,
  deleteFolder,
  exportFile,
  importFiles,
  isTauri,
  listFiles,
  listFolders,
  moveFiles,
  normalizeError,
  onFileDrop,
  onImportProgress,
  onBackupProgress,
  onRestoreProgress,
  onVaultUnavailable,
  previewImage,
  previewThumbnail,
  previewVideo,
  previewPdf,
  renameFile,
  renameFolder,
  restoreVault,
  setFavorite,
  verifyBackupPassword,
  verifyFiles,
} from "../lib/vaultClient.js";

defineProps({
  vaultName: { type: String, default: "我的保险库" },
  vaultPath: { type: String, default: "" },
});
const emit = defineEmits(["lock", "disconnected", "appearance"]);

const PAGE_SIZE = 60;
const TYPE_OPTIONS = [
  { label: "全部", value: FILE_TYPE_ALL },
  { label: "图片", value: "image" },
  { label: "PDF", value: "pdf" },
  { label: "视频", value: "video" },
  { label: "其他", value: "other" },
];
const SORT_OPTIONS = [
  { label: "导入时间：新到旧", value: "importedAt:desc" },
  { label: "导入时间：旧到新", value: "importedAt:asc" },
  { label: "文件大小：大到小", value: "size:desc" },
  { label: "文件大小：小到大", value: "size:asc" },
];
/** FR-42：「根目录」在移动选择器中的稳定值，提交前转换为 null。 */
const ROOT_FOLDER_VALUE = "__root__";
const IDLE_FOLDER_DIALOG = Object.freeze({
  open: false,
  mode: "create",
  id: null,
  originalName: "",
  draft: "",
  busy: false,
  error: null,
});
const IDLE_MOVE_STATE = Object.freeze({
  open: false,
  ids: [],
  names: [],
  target: ROOT_FOLDER_VALUE,
  busy: false,
});
/** FR-46：恢复时验证备份主密码的对话框状态。 */
const IDLE_RESTORE_DIALOG = Object.freeze({
  open: false,
  backupDir: "",
  password: "",
  busy: false,
  error: null,
});

const message = useMessage();

const listState = ref({ items: [], loading: false, error: null });
const browseState = ref({
  total: 0,
  matched: [],
  visible: [],
  hiddenCount: 0,
  visibleCount: PAGE_SIZE,
  query: { ...DEFAULT_QUERY },
  isFiltering: false,
});
const importing = ref(false);
const dropState = ref({ active: false, count: 0 });
const viewMode = ref("list");
const thumbnailState = ref({});
const progress = ref(null);
const summary = ref(null);
const cancelRequested = ref(false);
const verifying = ref(false);
const verifyReport = ref(null);
const highlighted = ref(new Set());
const detailsState = ref(null);
const exportingId = ref(null);
const previewState = ref({
  status: "idle",
  id: null,
  name: null,
  url: null,
  message: "",
  code: null,
});
const pdfState = ref({
  status: "idle",
  id: null,
  name: null,
  bytes: null,
  message: "",
  code: null,
});
const videoState = ref({
  status: "idle",
  id: null,
  name: null,
  url: null,
  message: "",
  code: null,
});
const deleteState = ref({ target: null, busy: false });
const renameState = ref({ target: null, draft: "", busy: false, error: null });
const selectionState = ref({ ids: [], idSet: new Set(), count: 0 });
const batchState = ref({ open: false, ids: [], total: 0, busy: false, processed: 0 });
const batchExportState = ref({
  status: "idle",
  total: 0,
  processed: 0,
  succeeded: 0,
  failed: 0,
});
const batchExportResult = ref(null);
const favoriteBusy = ref(new Set());
const folders = ref([]);
const folderActionBusy = ref(false);
const folderDialog = ref({ ...IDLE_FOLDER_DIALOG });
const moveState = ref({ ...IDLE_MOVE_STATE });
const backupState = ref({ status: "idle", processed: 0, total: 0, bytes: 0 });
const backupResult = ref(null);
const restoreState = ref({ status: "idle", processed: 0, total: 0, bytes: 0 });
const restoreResult = ref(null);
const restoreDialog = ref({ ...IDLE_RESTORE_DIALOG });

let alive = true;
let unlisten = null;
let unlistenDrop = null;
let unlistenBackup = null;
let unlistenRestore = null;
let stopVaultUnavailableWatch = null;
let lastItems = null;
let exportDialogOpen = false;
let selectStemPending = false;
const highlightTimers = new Map();

const files = computed(() => listState.value.items);
const sortValue = computed(
  () => `${browseState.value.query.sortKey}:${browseState.value.query.sortDirection}`
);
const trimmedKeyword = computed(() => normalizeKeyword(browseState.value.query.keyword));
const activeFolderId = computed(() => browseState.value.query.folderId ?? null);
const activeFolder = computed(
  () => folders.value.find((folder) => folder.id === activeFolderId.value) || null
);
const folderCounts = computed(() => countFilesByFolder(files.value));
const moveOptions = computed(() => [
  { label: "根目录", value: ROOT_FOLDER_VALUE },
  ...folders.value.map((folder) => ({ label: folder.name, value: folder.id })),
]);

const selection = createSelectionController({
  notify: (next) => {
    if (!alive) return;
    selectionState.value = next;
  },
});

const browser = createFileBrowser({
  pageSize: PAGE_SIZE,
  onChange: (next) => {
    if (!alive) return;
    browseState.value = next;
    // 搜索、筛选、排序或刷新后，隐藏条目的选择立即失效。
    selection.reconcile(next.visible.map((file) => file.id));
  },
});

const dragDrop = createDragDropController({
  notify: (next) => {
    if (!alive) return;
    dropState.value = next;
  },
});

const details = createFileDetails({
  onChange: (next) => {
    if (!alive) return;
    detailsState.value = next;
  },
});

const listStore = createFileListStore({
  fetchFiles: () => listFiles(),
  notify: (next) => {
    if (!alive) return;
    listState.value = next;
    // 仅在文件集合真正变化时重置分页；加载中/错误状态不应打断“显示更多”。
    if (next.items !== lastItems) {
      lastItems = next.items;
      browser.setFiles(next.items);
      // 列表刷新后，详情目标已不存在则自动关闭，存在则同步为最新元数据。
      details.sync(next.items);
    }
    if (next.error?.code === "NOT_UNLOCKED") {
      closeDetails();
      closeAllPreviews();
      emit("lock");
    }
  },
});

const preview = createPreviewController({
  loadBytes: (id) => previewImage(id),
  createObjectUrl: (blob) => URL.createObjectURL(blob),
  revokeObjectUrl: (url) => URL.revokeObjectURL(url),
  notify: (next) => {
    if (!alive) return;
    previewState.value = next;
    if (next.code === "NOT_UNLOCKED") {
      preview.close();
      closeDetails();
      emit("lock");
    }
  },
});

const pdfPreview = createPdfPreviewController({
  loadBytes: (id) => previewPdf(id),
  notify: (next) => {
    if (!alive) return;
    pdfState.value = next;
    if (next.code === "NOT_UNLOCKED") {
      pdfPreview.close();
      closeDetails();
      preview.close();
      emit("lock");
    }
  },
});

const videoPreview = createVideoPreviewController({
  loadBytes: (id) => previewVideo(id),
  createObjectUrl: (blob) => URL.createObjectURL(blob),
  revokeObjectUrl: (url) => URL.revokeObjectURL(url),
  notify: (next) => {
    if (!alive) return;
    videoState.value = next;
    if (next.code === "NOT_UNLOCKED") {
      videoPreview.close();
      closeDetails();
      preview.close();
      pdfPreview.close();
      emit("lock");
    }
  },
});

const thumbnails = createThumbnailController({
  loadBytes: (id) => previewThumbnail(id),
  createObjectUrl: (blob) => URL.createObjectURL(blob),
  revokeObjectUrl: (url) => URL.revokeObjectURL(url),
  isImage: (name) => previewTypeFor(name) !== null,
  notify: (next) => {
    if (!alive) return;
    thumbnailState.value = next;
  },
});

const deletion = createDeleteController({
  deleteById: (id) => deleteFile(id),
  notify: (next) => {
    if (!alive) return;
    deleteState.value = next;
  },
});

const batchDeletion = createBatchDeleteController({
  deleteById: (id) => deleteFile(id),
  notify: (next) => {
    if (!alive) return;
    batchState.value = next;
  },
});

const batchExport = createBatchExportController({
  exportById: (id, targetPath) => exportFile(id, targetPath),
  notify: (next) => {
    if (!alive) return;
    batchExportState.value = next;
  },
});

const allVisibleSelected = computed(() => {
  const visible = browseState.value.visible;
  return visible.length > 0 && visible.every((file) => selectionState.value.idSet.has(file.id));
});

const renaming = createRenameController({
  renameById: (id, name) => renameFile(id, name),
  notify: (next) => {
    if (!alive) return;
    renameState.value = next;
  },
});

function isImageFile(file) {
  return previewTypeFor(file?.name) !== null;
}

function loadVisibleThumbnails() {
  if (viewMode.value !== "grid") return;
  for (const file of browseState.value.visible) thumbnails.request(file);
}

/** 列表/网格共用同一份筛选、排序与分页结果，切换时只更换呈现方式。 */
function setViewMode(mode) {
  if (mode !== "grid" && mode !== "list") return;
  if (mode === viewMode.value) return;
  viewMode.value = mode;
  if (mode === "grid") {
    loadVisibleThumbnails();
  } else {
    thumbnails.releaseAll();
  }
}

watch(() => browseState.value.visible, loadVisibleThumbnails);

function highlight(id) {
  const next = new Set(highlighted.value);
  next.add(id);
  highlighted.value = next;
  const timer = window.setTimeout(() => {
    const cleared = new Set(highlighted.value);
    cleared.delete(id);
    highlighted.value = cleared;
    highlightTimers.delete(id);
  }, 2400);
  highlightTimers.set(id, timer);
}

/** FR-42：文件夹列表同样来自加密索引，锁定后不可读取。 */
async function loadFolders() {
  if (!isTauri()) return;
  try {
    const items = await listFolders();
    if (!alive) return;
    folders.value = Array.isArray(items) ? items : [];
    // 当前文件夹已在其他流程中被删除时退回全部文件，避免停留在不存在的范围。
    if (activeFolderId.value && !folders.value.some((folder) => folder.id === activeFolderId.value)) {
      browser.setQuery({ folderId: null });
    }
  } catch (err) {
    if (!alive) return;
    const normalized = normalizeError(err);
    if (normalized.code === "NOT_UNLOCKED" || normalized.code === "VAULT_RECOVERY_REQUIRED") {
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    message.error(normalized.message);
  }
}

function refresh({ silent = false } = {}) {
  if (!isTauri()) return Promise.resolve();
  return Promise.all([listStore.load(), loadFolders()]).then(() => {
    if (!alive || silent) return;
    const error = listState.value.error;
    if (error && error.code !== "NOT_UNLOCKED") message.error(error.message);
  });
}

function showMore() {
  browser.showMore();
}

function handleKeyword(value) {
  browser.setQuery({ keyword: value });
}

function handleTypeChange(value) {
  browser.setQuery({ type: value });
}

function handleSortChange(value) {
  const [sortKey, sortDirection] = String(value).split(":");
  browser.setQuery({ sortKey, sortDirection });
}

function handleClearFilters() {
  browser.clearFilters();
}

function setFavoriteFilter(value) {
  browser.setQuery({ favoriteOnly: value === true });
}

/** 进入全部文件或指定文件夹；只改变浏览范围，不触碰文件数据。 */
function selectFolder(folderId) {
  browser.setQuery({ folderId: typeof folderId === "string" ? folderId : null });
}

function openCreateFolder() {
  if (folderDialog.value.busy || folderActionBusy.value) return;
  folderDialog.value = { ...IDLE_FOLDER_DIALOG, open: true };
}

function openRenameFolder(folder) {
  if (folderDialog.value.busy || folderActionBusy.value) return;
  folderDialog.value = {
    ...IDLE_FOLDER_DIALOG,
    open: true,
    mode: "rename",
    id: folder.id,
    originalName: folder.name,
    draft: folder.name,
  };
}

function handleFolderDraft(value) {
  folderDialog.value = { ...folderDialog.value, draft: value, error: null };
}

function closeFolderDialog() {
  if (folderDialog.value.busy) return;
  folderDialog.value = { ...IDLE_FOLDER_DIALOG };
}

function handleFolderKeydown(event) {
  if (event?.key !== "Enter" || event.isComposing) return;
  event.preventDefault();
  confirmFolderDialog();
}

/** 创建/重命名文件夹：只有后端确认索引写入成功后才更新本地列表。 */
async function confirmFolderDialog() {
  const dialog = folderDialog.value;
  if (!dialog.open || dialog.busy) return;
  const name = dialog.draft.trim();
  if (!name) {
    folderDialog.value = { ...dialog, error: { message: "文件夹名称不能为空。" } };
    return;
  }

  folderDialog.value = { ...dialog, busy: true, error: null };
  try {
    const saved =
      dialog.mode === "create"
        ? await createFolder(name)
        : await renameFolder(dialog.id, name);
    if (!alive) return;
    if (dialog.mode === "create") {
      folders.value = [...folders.value, saved];
    } else {
      folders.value = folders.value.map((folder) => (folder.id === saved.id ? saved : folder));
    }
    folders.value = [...folders.value].sort((left, right) =>
      left.name.toLowerCase().localeCompare(right.name.toLowerCase())
    );
    folderDialog.value = { ...IDLE_FOLDER_DIALOG };
    message.success(
      dialog.mode === "create" ? `已创建文件夹「${saved.name}」。` : `已重命名为「${saved.name}」。`
    );
  } catch (err) {
    if (!alive) return;
    const normalized = normalizeError(err);
    if (normalized.code === "NOT_UNLOCKED" || normalized.code === "VAULT_RECOVERY_REQUIRED") {
      folderDialog.value = { ...IDLE_FOLDER_DIALOG };
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    folderDialog.value = {
      ...folderDialog.value,
      busy: false,
      error: { code: normalized.code, message: normalized.message },
    };
  }
}

/** 删除文件夹：前端先按当前数量拦截非空文件夹，后端仍会再次校验。 */
async function handleDeleteFolder(folder) {
  if (folderActionBusy.value || folderDialog.value.busy) return;
  const count = folderCounts.value[folder.id] || 0;
  if (count > 0) {
    message.warning(`文件夹「${folder.name}」中还有 ${count} 个文件，无法删除。`);
    return;
  }

  folderActionBusy.value = true;
  try {
    await deleteFolder(folder.id);
    if (!alive) return;
    folders.value = folders.value.filter((item) => item.id !== folder.id);
    if (activeFolderId.value === folder.id) browser.setQuery({ folderId: null });
    message.success(`已删除空文件夹「${folder.name}」。`);
  } catch (err) {
    if (!alive) return;
    const normalized = normalizeError(err);
    if (normalized.code === "NOT_UNLOCKED" || normalized.code === "VAULT_RECOVERY_REQUIRED") {
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    message.error(normalized.message);
  } finally {
    if (alive) folderActionBusy.value = false;
  }
}

function openMoveDialog(targetFiles) {
  const list = (Array.isArray(targetFiles) ? targetFiles : []).filter(
    (file) => file && typeof file.id === "string"
  );
  if (list.length === 0) return;
  moveState.value = {
    open: true,
    ids: list.map((file) => file.id),
    names: list.map((file) => file.name),
    target: ROOT_FOLDER_VALUE,
    busy: false,
  };
}

function requestMove(file) {
  if (moveState.value.busy || folderDialog.value.busy) return;
  openMoveDialog([file]);
}

function requestBatchMove() {
  if (moveState.value.busy || folderDialog.value.busy) return;
  openMoveDialog(
    selectionState.value.ids
      .map((id) => files.value.find((file) => file.id === id))
      .filter(Boolean)
  );
}

function handleMoveTarget(value) {
  moveState.value = { ...moveState.value, target: value };
}

function cancelMove() {
  if (moveState.value.busy) return;
  moveState.value = { ...IDLE_MOVE_STATE };
}

/** 移动文件：整批索引写入成功后逐个更新本地条目，失败时界面保持原状。 */
async function confirmMove() {
  const state = moveState.value;
  if (!state.open || state.busy) return;
  const folderId = state.target === ROOT_FOLDER_VALUE ? null : state.target;
  moveState.value = { ...state, busy: true };
  try {
    const updated = await moveFiles(state.ids, folderId);
    if (!alive) return;
    for (const item of Array.isArray(updated) ? updated : []) listStore.replace(item);
    const count = state.ids.length;
    moveState.value = { ...IDLE_MOVE_STATE };
    selection.clear();
    message.success(
      folderId ? `已将 ${count} 个文件移动到文件夹。` : `已将 ${count} 个文件移回根目录。`
    );
  } catch (err) {
    if (!alive) return;
    const normalized = normalizeError(err);
    if (normalized.code === "NOT_UNLOCKED" || normalized.code === "VAULT_RECOVERY_REQUIRED") {
      moveState.value = { ...IDLE_MOVE_STATE };
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    moveState.value = { ...moveState.value, busy: false };
    message.error(normalized.message || "移动失败，请重试。");
  }
}

/** 收藏/取消收藏：仅在 Rust 确认索引写入成功后更新界面，失败保持原状态。 */
async function handleToggleFavorite(file) {
  if (favoriteBusy.value.has(file.id)) return;
  const nextFavorite = file.favorite !== true;
  favoriteBusy.value = new Set(favoriteBusy.value).add(file.id);
  try {
    const updated = await setFavorite(file.id, nextFavorite);
    if (!alive) return;
    listStore.replace(updated);
  } catch (err) {
    if (!alive) return;
    const normalized = normalizeError(err);
    if (normalized.code === "NOT_UNLOCKED" || normalized.code === "VAULT_RECOVERY_REQUIRED") {
      closeAllPreviews();
      closeDetails();
      emit("lock");
      return;
    }
    message.error(normalized.message || "收藏操作失败，请重试。");
  } finally {
    if (alive) {
      const remaining = new Set(favoriteBusy.value);
      remaining.delete(file.id);
      favoriteBusy.value = remaining;
    }
  }
}

function handleFileClick(file) {
  if (isPdfName(file.name)) {
    closeDetails();
    preview.close();
    videoPreview.close();
    pdfPreview.open(file);
    return;
  }
  if (isVideoName(file.name)) {
    closeDetails();
    preview.close();
    pdfPreview.close();
    videoPreview.open(file);
    return;
  }
  if (!previewTypeFor(file.name)) {
    message.info("该文件类型暂不支持预览。");
    return;
  }
  closeDetails();
  pdfPreview.close();
  videoPreview.close();
  preview.open(file);
}

function handleDetailsClick(file) {
  if (previewState.value.status !== "idle") preview.close();
  if (pdfState.value.status !== "idle") pdfPreview.close();
  if (videoState.value.status !== "idle") videoPreview.close();
  details.open(file);
}

async function handleExport(file) {
  // 系统对话框打开期间与导出进行中都阻止重复任务。
  if (exportDialogOpen || exportingId.value !== null) return;
  exportDialogOpen = true;
  try {
    const target = await chooseSavePath(file.name, `导出「${file.name}」`);
    if (!target || !alive) return;

    exportingId.value = file.id;
    try {
      const result = await exportFile(file.id, target);
      if (!alive) return;
      if (result?.warning) {
        // rename 已成功但目录持久化未确认：不能当作普通成功。
        message.warning(exportDurabilityWarning(result.warning));
      } else {
        message.success(singleExportSuccess(file.name));
      }
    } catch (err) {
      if (!alive) return;
      const normalized = normalizeError(err);
      if (normalized.code === "NOT_UNLOCKED") {
        closeDetails();
        closeAllPreviews();
        emit("lock");
        return;
      }
      if (normalized.code === "EXPORT_CANCELLED") {
        message.info(normalized.message);
        return;
      }
      message.error(normalized.message);
    } finally {
      exportingId.value = null;
    }
  } catch (err) {
    if (!alive) return;
    message.error(normalizeError(err).message);
  } finally {
    exportDialogOpen = false;
  }
}

function requestDelete(file) {
  deletion.open(file);
}

function cancelDelete() {
  deletion.cancel();
}

async function confirmDelete() {
  const outcome = await deletion.confirm();
  if (!alive || !outcome) return;
  const { file } = outcome;

  if (outcome.status === "failed") {
    const normalized = normalizeError(outcome.error);
    if (normalized.code === "VAULT_RECOVERY_REQUIRED") {
      message.error("删除状态尚未确认，请重新解锁以恢复保险库。");
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    if (normalized.code === "NOT_UNLOCKED") {
      message.error(normalized.message);
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    if (normalized.code === "DELETE_BUSY") {
      message.warning(normalized.message);
      return;
    }
    if (normalized.code === "DELETE_CANCELLED") {
      message.info(normalized.message);
      return;
    }
    message.error(normalized.message);
    return;
  }

  // 只有后端明确提交成功才移除条目；旧加载请求不能把它恢复回来。
  listStore.remove(file.id);
  if (details.getState()?.id === file.id) details.close();
  if (preview.getState().id === file.id) preview.close();
  if (pdfPreview.getState().id === file.id) pdfPreview.close();
  if (videoPreview.getState().id === file.id) videoPreview.close();
  thumbnails.release(file.id);

  if (outcome.result.cleanupPending) {
    message.warning(`已从 Haven 移除「${file.name}」；加密临时数据将在下次解锁时继续清理。`);
  } else {
    message.success(`已从 Haven 移除「${file.name}」。`);
  }

  await refresh({ silent: true });
  if (!alive) return;
  const error = listState.value.error;
  if (!error || error.code === "NOT_UNLOCKED") return;
  if (error.code === "VAULT_RECOVERY_REQUIRED") {
    closeDetails();
    closeAllPreviews();
    emit("lock");
    return;
  }
  message.warning("文件已经移除，但列表刷新失败；其余列表数据和查询条件已保留。");
}

function handleToggleSelect(file, checked) {
  selection.toggle(file.id, checked === true);
}

function handleToggleSelectAll() {
  if (allVisibleSelected.value) {
    selection.clear();
    return;
  }
  selection.selectAll(browseState.value.visible.map((file) => file.id));
}

function requestBatchDelete() {
  if (deleteState.value.busy || renameState.value.busy || batchState.value.busy) return;
  batchDeletion.request(selectionState.value.ids);
}

function cancelBatchDelete() {
  batchDeletion.cancel();
}

function selectedFilesForExport() {
  return selectionState.value.ids
    .map((id) => files.value.find((file) => file.id === id))
    .filter(Boolean)
    .map((file) => ({ id: file.id, name: file.name }));
}

function batchExportBlocked() {
  return (
    importing.value ||
    exportDialogOpen ||
    exportingId.value !== null ||
    deleteState.value.busy ||
    renameState.value.busy ||
    batchState.value.busy ||
    folderActionBusy.value ||
    folderDialog.value.busy ||
    moveState.value.busy
  );
}

/** 导出结果提示：单文件失败继续、取消保留已导出、锁定/恢复时进入锁定流程。 */
function applyBatchExportOutcome(outcome, label) {
  batchExportResult.value = {
    status: outcome.status,
    succeeded: outcome.succeeded.length,
    failures: outcome.failures,
    cancelled: outcome.status === "cancelled",
    interrupted: outcome.status === "interrupted",
  };

  const ok = outcome.succeeded.length;
  const failed = outcome.failures.length;

  if (outcome.status === "interrupted") {
    message.error(`${label}已停止：成功 ${ok} 个，失败 ${failed} 个。已导出的文件保留。`);
    closeDetails();
    closeAllPreviews();
    emit("lock");
    return;
  }
  if (outcome.status === "cancelled") {
    if (ok === 0 && failed === 0) {
      message.info(`已取消${label}，未导出任何文件。`);
    } else {
      message.info(
        `已取消${label}：成功 ${ok} 个${failed > 0 ? `，失败 ${failed} 个` : ""}。已导出的文件保留。`
      );
    }
    return;
  }
  if (failed > 0) {
    message.warning(batchExportPartial(label, ok, failed));
    return;
  }
  message.success(batchExportSuccess(label, ok));
}

/** FR-37：选择目录后按现有单文件导出命令串行导出，逐个复用导出事务。 */
async function handleBatchExport() {
  if (batchExportState.value.status === "running" || batchExportBlocked()) return;

  const items = selectedFilesForExport();
  if (items.length === 0) return;

  const directory = await chooseDirectory(`选择 ${items.length} 个文件的导出目录`);
  if (!alive || !directory) return;

  batchExportResult.value = null;
  const outcome = await batchExport.start(items, directory);
  if (!alive || !outcome) return;

  selection.clear();
  applyBatchExportOutcome(outcome, "批量导出");
}

/** FR-43：导出当前文件夹内全部文件，复用同一批量导出控制器与导出事务。 */
async function handleFolderExport(folder) {
  if (batchExportState.value.status === "running" || batchExportBlocked()) return;

  const items = collectFolderExportItems(files.value, folder.id);
  if (items.length === 0) {
    message.info(`文件夹「${folder.name}」中还没有文件，无需导出。`);
    return;
  }

  const directory = await chooseDirectory(`选择文件夹「${folder.name}」的导出目录`);
  if (!alive || !directory) return;

  batchExportResult.value = null;
  const outcome = await batchExport.start(items, directory);
  if (!alive || !outcome) return;

  applyBatchExportOutcome(outcome, "文件夹导出");
}

function cancelBatchExport() {
  batchExport.cancel();
}

async function confirmBatchDelete() {
  const outcome = await batchDeletion.confirm();
  if (!alive || !outcome) return;

  // 成功项立即本地移除；失败项保留，最终以静默刷新结果为准。
  for (const id of outcome.succeeded) {
    listStore.remove(id);
    if (details.getState()?.id === id) details.close();
    if (preview.getState().id === id) preview.close();
    if (pdfPreview.getState().id === id) pdfPreview.close();
    if (videoPreview.getState().id === id) videoPreview.close();
    thumbnails.release(id);
  }
  selection.clear();

  if (outcome.fatal) {
    if (outcome.succeeded.length > 0) {
      message.warning(`已删除 ${outcome.succeeded.length} 个文件；其余文件未处理。`);
    }
    if (outcome.fatal.code === "NOT_UNLOCKED") {
      message.error("保险库已锁定，批量删除已停止。");
    } else {
      message.error("删除状态尚未确认，请重新解锁以恢复保险库。");
    }
    closeDetails();
    closeAllPreviews();
    emit("lock");
    return;
  }

  if (outcome.failures.length > 0) {
    // 部分成功必须如实报告数量，不能当作全部成功。
    message.warning(
      `已删除 ${outcome.succeeded.length} 个文件，${outcome.failures.length} 个删除失败；失败文件仍保留在保险库中。`
    );
  } else if (outcome.cleanupPending) {
    message.warning(
      `已删除 ${outcome.succeeded.length} 个文件；部分加密临时数据将在下次解锁时继续清理。`
    );
  } else {
    message.success(`已删除 ${outcome.succeeded.length} 个文件。`);
  }

  await refresh({ silent: true });
  if (!alive) return;
  const error = listState.value.error;
  if (!error || error.code === "NOT_UNLOCKED") return;
  if (error.code === "VAULT_RECOVERY_REQUIRED") {
    closeDetails();
    closeAllPreviews();
    emit("lock");
    return;
  }
  message.warning("批量删除已完成，但列表刷新失败；请重试刷新以核对剩余文件。");
}

function requestRename(file) {
  if (!renaming.open(file)) return;
  selectStemPending = true;
}

function handleRenameInput(value) {
  renaming.setDraft(value);
}

/** 打开对话框后默认选中主文件名，扩展名保持未选中。 */
function handleRenameFocus(event) {
  if (!selectStemPending) return;
  selectStemPending = false;
  const input = event?.target;
  if (!input || typeof input.setSelectionRange !== "function") return;
  const { start, end } = baseNameSelection(input.value);
  input.setSelectionRange(start, end);
}

function handleRenameKeydown(event) {
  if (event?.key !== "Enter" || event.isComposing) return;
  event.preventDefault();
  confirmRename();
}

function cancelRename() {
  if (renaming.cancel()) selectStemPending = false;
}

async function confirmRename() {
  if (renameState.value.busy) return;
  const outcome = await renaming.confirm();
  if (!alive || !outcome) return;

  // 前端校验失败：错误已显示在对话框，不发送 IPC。
  if (outcome.status === "invalid") return;

  if (outcome.status === "failed") {
    const normalized = normalizeError(outcome.error);
    if (normalized.code === "NOT_UNLOCKED") {
      cancelRename();
      message.error(normalized.message);
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    if (normalized.code === "VAULT_RECOVERY_REQUIRED") {
      cancelRename();
      message.error("重命名状态尚未确认，请重新解锁以恢复保险库。");
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    if (normalized.code === "RENAME_BUSY") {
      message.warning(normalized.message);
      return;
    }
    message.error(normalized.message);
    return;
  }

  // 后端明确提交成功后才更新列表；详情会跟随最新名称。
  const updated = { id: outcome.file.id, name: outcome.file.name };
  listStore.replace(updated);
  // 名称可能改变扩展名，重新判断是否需要缩略图。
  thumbnails.release(updated.id);
  if (viewMode.value === "grid" && isImageFile(updated)) thumbnails.request(updated);
  if (previewState.value.id === updated.id) preview.close();
  if (videoState.value.id === updated.id) videoPreview.close();
  if (pdfState.value.id === updated.id) pdfPreview.close();
  message.success(`已重命名为「${updated.name}」。`);
}

function closeDetails() {
  details.close();
}

function closePreview() {
  preview.close();
}

function closePdfPreview() {
  pdfPreview.close();
}

function closeVideoPreview() {
  videoPreview.close();
}

/** 锁定、卸载和不可恢复错误时同时释放两类预览的内存引用。 */
function closeAllPreviews() {
  preview.close();
  pdfPreview.close();
  videoPreview.close();
}

function handleProgress(event) {
  if (!alive) return;
  progress.value = event;
  if (event.phase === "success") {
    const entry = files.value.find((file) => file.name === event.name);
    if (entry) highlight(entry.id);
  }
}

/** 文件选择器与拖拽导入共用的真实导入流程。 */
async function runImport(paths) {
  if (importing.value) {
    message.warning("正在导入文件，请等待当前导入完成后再试。");
    return;
  }
  if (!paths.length) return;

  importing.value = true;
  cancelRequested.value = false;
  summary.value = null;
  verifyReport.value = null;
  progress.value = null;

  try {
    const result = await importFiles(paths);
    if (!alive) return;
    summary.value = result;
    if (result.cancelled) {
      message.info("导入已取消，未完成的文件已丢弃。");
    } else if (result.failed.length > 0) {
      message.warning(
        `已导入 ${result.succeeded.length} 个文件，${result.failed.length} 个失败。`
      );
    } else if (result.succeeded.length === 0) {
      // 空文件夹或只包含子空目录：不产生任何数据变化。
      message.info("没有找到可导入的普通文件；文件夹可能为空。");
    } else {
      message.success(`已导入 ${result.succeeded.length} 个文件。`);
    }
    for (const item of result.succeeded) highlight(item.id);
    await refresh({ silent: true });
  } catch (err) {
    if (!alive) return;
    const normalized = normalizeError(err);
    if (normalized.code === "VAULT_RECOVERY_REQUIRED") {
      message.error("导入状态尚未确认，请重新解锁以恢复保险库。");
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    if (normalized.code === "NOT_UNLOCKED") {
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    summary.value = {
      succeeded: [],
      failed: [{ name: "本次导入", code: normalized.code, message: normalized.message }],
      cancelled: false,
    };
    message.error(normalized.message);
  } finally {
    importing.value = false;
    progress.value = null;
  }
}

async function handleImport() {
  if (importing.value) return;
  const paths = await chooseFiles("选择要导入保险库的文件");
  if (!paths.length) return;
  await runImport(paths);
}

/** FR-38：选择一个或多个文件夹，由 Rust 递归展开后走同一导入流程。 */
async function handleImportFolders() {
  if (importing.value) return;
  const paths = await chooseDirectories("选择要导入保险库的文件夹");
  if (!paths.length) return;
  await runImport(paths);
}

/** 拖拽松开：先结束提示，再进入与文件选择器一致的导入流程。 */
function handleDropped(paths) {
  const dropped = dragDrop.drop(paths);
  if (!dropped.length) {
    message.info("没有检测到可导入的文件或文件夹。");
    return;
  }
  runImport(dropped);
}

function handleFileDrag(event) {
  if (!event || typeof event.type !== "string") return;
  if (event.type === "enter") {
    dragDrop.enter(event.paths);
  } else if (event.type === "over") {
    dragDrop.over();
  } else if (event.type === "leave") {
    dragDrop.leave();
  } else if (event.type === "drop") {
    handleDropped(event.paths);
  }
}

async function handleCancel() {
  if (!importing.value || cancelRequested.value) return;
  cancelRequested.value = true;
  try {
    await cancelImport();
    message.info("正在安全停止导入…");
  } catch (err) {
    cancelRequested.value = false;
    message.error(normalizeError(err).message);
  }
}

async function handleVerify() {
  if (verifying.value) return;
  verifying.value = true;
  verifyReport.value = null;
  try {
    const report = await verifyFiles();
    if (!alive) return;
    verifyReport.value = report;
    if (report.failed.length === 0 && !report.cancelled) {
      message.success(`已校验 ${report.checked} 个文件，全部完整。`);
    } else if (report.cancelled) {
      message.info("完整性校验已取消。");
    } else {
      message.error(`${report.failed.length} 个文件未通过完整性校验。`);
    }
  } catch (err) {
    if (!alive) return;
    message.error(normalizeError(err).message);
  } finally {
    verifying.value = false;
  }
}

function handleBackupProgress(progress) {
  if (!alive || backupState.value.status !== "running") return;
  backupState.value = {
    status: "running",
    processed: progress?.processed ?? 0,
    total: progress?.total ?? 0,
    bytes: progress?.bytes ?? 0,
  };
}

/** FR-44：选择目录后复制加密数据；取消选择不执行任何操作。 */
async function handleBackupVault() {
  if (backupState.value.status === "running") return;
  if (restoreState.value.status === "running") {
    message.warning("请等待当前恢复操作完成后再备份保险库。");
    return;
  }
  if (
    importing.value ||
    verifying.value ||
    exportDialogOpen ||
    exportingId.value !== null ||
    deleteState.value.busy ||
    renameState.value.busy ||
    batchState.value.busy ||
    batchExportState.value.status === "running" ||
    folderActionBusy.value ||
    folderDialog.value.busy ||
    moveState.value.busy
  ) {
    message.warning("请等待当前文件操作完成后再备份保险库。");
    return;
  }

  const target = await chooseDirectory("选择保险库备份位置");
  if (!alive || !target) return;

  backupResult.value = null;
  backupState.value = { status: "running", processed: 0, total: 0, bytes: 0 };
  try {
    const summary = await backupVault(target);
    if (!alive) return;
    backupState.value = { status: "idle", processed: 0, total: 0, bytes: 0 };
    backupResult.value = {
      destination: summary.destination,
      fileCount: summary.fileCount,
      totalBytes: summary.totalBytes,
    };
    message.success(`保险库已备份：${summary.fileCount} 个加密文件。`);
  } catch (err) {
    if (!alive) return;
    backupState.value = { status: "idle", processed: 0, total: 0, bytes: 0 };
    const normalized = normalizeError(err);
    if (normalized.code === "NOT_UNLOCKED" || normalized.code === "VAULT_RECOVERY_REQUIRED") {
      closeDetails();
      closeAllPreviews();
      emit("lock");
      return;
    }
    message.error(normalized.message || "备份失败，未生成完整备份。");
  }
}

function handleRestoreProgress(progress) {
  if (!alive || restoreState.value.status !== "running") return;
  restoreState.value = {
    status: "running",
    processed: progress?.processed ?? 0,
    total: progress?.total ?? 0,
    bytes: progress?.bytes ?? 0,
  };
}

/** FR-45/46：选择备份目录后输入该备份的主密码；取消任一步即退出，不产生恢复结果。 */
async function handleRestoreVault() {
  if (restoreState.value.status === "running") return;
  if (backupState.value.status === "running") {
    message.warning("请等待当前备份操作完成后再恢复保险库。");
    return;
  }
  if (
    importing.value ||
    verifying.value ||
    exportDialogOpen ||
    exportingId.value !== null ||
    deleteState.value.busy ||
    renameState.value.busy ||
    batchState.value.busy ||
    batchExportState.value.status === "running" ||
    folderActionBusy.value ||
    folderDialog.value.busy ||
    moveState.value.busy
  ) {
    message.warning("请等待当前文件操作完成后再恢复保险库。");
    return;
  }

  const backupDir = await chooseDirectory("选择保险库备份目录");
  if (!alive || !backupDir) return;

  restoreResult.value = null;
  restoreDialog.value = { ...IDLE_RESTORE_DIALOG, open: true, backupDir };
}

function handleRestorePasswordDraft(value) {
  restoreDialog.value = { ...restoreDialog.value, password: value, error: null };
}

function closeRestoreDialog() {
  // 验证进行中吞掉关闭动作，避免表现为“已取消”。
  if (restoreDialog.value.busy) return;
  restoreDialog.value = { ...IDLE_RESTORE_DIALOG };
}

function handleRestoreDialogKeydown(event) {
  if (event.key === "Escape") {
    event.preventDefault();
    closeRestoreDialog();
    return;
  }
  if (event.key === "Enter" && !restoreDialog.value.busy) {
    event.preventDefault();
    confirmRestorePassword();
  }
}

/** FR-46：先用备份自身的参数验证主密码，通过后再选择恢复位置执行恢复。 */
async function confirmRestorePassword() {
  const dialog = restoreDialog.value;
  if (dialog.busy || !dialog.open) return;
  if (!dialog.password) {
    restoreDialog.value = {
      ...dialog,
      error: { message: "请输入创建该备份的保险库的主密码。" },
    };
    return;
  }

  restoreDialog.value = { ...dialog, busy: true, error: null };
  try {
    await verifyBackupPassword(dialog.backupDir, dialog.password);
  } catch (err) {
    if (!alive) return;
    const normalized = normalizeError(err);
    restoreDialog.value = {
      ...restoreDialog.value,
      busy: false,
      error: { message: normalized.message || "主密码验证未通过。" },
    };
    return;
  }
  if (!alive) return;

  const { backupDir, password } = restoreDialog.value;
  restoreDialog.value = { ...IDLE_RESTORE_DIALOG };

  const targetDir = await chooseDirectory("选择恢复位置（将在其中创建「我的保险库」）");
  if (!alive || !targetDir) return;

  restoreState.value = { status: "running", processed: 0, total: 0, bytes: 0 };
  try {
    const summary = await restoreVault(backupDir, targetDir, password);
    if (!alive) return;
    restoreState.value = { status: "idle", processed: 0, total: 0, bytes: 0 };
    restoreResult.value = {
      destination: summary.destination,
      fileCount: summary.fileCount,
      totalBytes: summary.totalBytes,
    };
    message.success(`保险库已恢复到新位置：${summary.fileCount} 个加密文件。`);
  } catch (err) {
    if (!alive) return;
    restoreState.value = { status: "idle", processed: 0, total: 0, bytes: 0 };
    const normalized = normalizeError(err);
    message.error(normalized.message || "恢复失败，未生成完整保险库。");
  }
}

function resetTransientUi() {
  // 未提交的删除/重命名/移动确认随会话结束关闭；执行中的操作由后端等待到安全退出点。
  deletion.cancel();
  cancelRename();
  batchDeletion.cancel();
  batchExport.cancel();
  folderDialog.value = { ...IDLE_FOLDER_DIALOG };
  restoreDialog.value = { ...IDLE_RESTORE_DIALOG };
  moveState.value = { ...IDLE_MOVE_STATE };
  dragDrop.leave();
  thumbnails.releaseAll();
  closeDetails();
  closeAllPreviews();
}

function handleLock() {
  resetTransientUi();
  emit("lock");
}

/**
 * 后端已因磁盘不可用清空主密钥，这里清空所有不可信的展示状态，
 * 并请求父组件从后端重新读取真实状态，避免本地伪造锁定。
 */
function handleVaultUnavailable() {
  if (!alive) return;
  resetTransientUi();
  listStore.clear();
  folders.value = [];
  selection.clear();
  emit("disconnected");
}

function handleKeydown(event) {
  if (event.key !== "Escape") return;
  // 删除确认框优先；执行中吞掉 Esc，绝不表现为“已取消”。
  // 批量删除确认框优先；执行中吞掉 Esc。
  if (batchState.value.open) {
    event.preventDefault();
    event.stopPropagation();
    batchDeletion.cancel();
    return;
  }
  if (deleteState.value.target) {
    event.preventDefault();
    event.stopPropagation();
    deletion.cancel();
    return;
  }
  // 重命名对话框：保存中吞掉 Esc，避免表现为“已取消”。
  if (renameState.value.target) {
    event.preventDefault();
    event.stopPropagation();
    cancelRename();
    return;
  }
  if (folderDialog.value.open) {
    event.preventDefault();
    event.stopPropagation();
    closeFolderDialog();
    return;
  }
  if (restoreDialog.value.open) {
    event.preventDefault();
    event.stopPropagation();
    closeRestoreDialog();
    return;
  }
  if (moveState.value.open) {
    event.preventDefault();
    event.stopPropagation();
    cancelMove();
    return;
  }
  if (detailsState.value) {
    closeDetails();
    return;
  }
  if (
    previewState.value.status !== "idle" ||
    pdfState.value.status !== "idle" ||
    videoState.value.status !== "idle"
  ) {
    closeAllPreviews();
  }
}

onMounted(async () => {
  window.addEventListener("keydown", handleKeydown);
  stopVaultUnavailableWatch = onVaultUnavailable(handleVaultUnavailable);
  unlisten = await onImportProgress(handleProgress);
  unlistenDrop = await onFileDrop(handleFileDrag);
  unlistenBackup = await onBackupProgress(handleBackupProgress);
  unlistenRestore = await onRestoreProgress(handleRestoreProgress);
  await refresh({ silent: true });
});

onBeforeUnmount(() => {
  alive = false;
  deletion.dispose();
  batchDeletion.dispose();
  batchExport.dispose();
  thumbnails.dispose();
  selection.dispose();
  renaming.dispose();
  dragDrop.dispose();
  details.close();
  preview.dispose();
  pdfPreview.dispose();
  videoPreview.dispose();
  listStore.dispose();
  window.removeEventListener("keydown", handleKeydown);
  if (typeof stopVaultUnavailableWatch === "function") stopVaultUnavailableWatch();
  if (typeof unlisten === "function") unlisten();
  if (typeof unlistenDrop === "function") unlistenDrop();
  if (typeof unlistenBackup === "function") unlistenBackup();
  if (typeof unlistenRestore === "function") unlistenRestore();
  for (const timer of highlightTimers.values()) window.clearTimeout(timer);
  highlightTimers.clear();
});
</script>

<template>
  <section class="haven-card haven-card--wide haven-card--vault" aria-labelledby="vault-title">
    <div class="haven-vault__head">
      <h1
        id="vault-title"
        class="haven-section-title"
        :title="vaultPath ? `保险库位置：${vaultPath}` : undefined"
      >
        {{ vaultName }}
      </h1>
      <div class="haven-vault__head-actions">
        <button
          type="button"
          class="haven-link-button"
          @click="emit('appearance')"
        >
          外观
        </button>
        <span class="haven-vault__status">已解锁</span>
      </div>
    </div>

    <div v-if="dropState.active" class="haven-dropzone" role="status" aria-live="polite">
      <div class="haven-dropzone__panel">
        <p class="haven-dropzone__title">松开以导入 {{ dropState.count }} 个项目</p>
        <p class="haven-dropzone__hint">
          文件夹会递归导入其中的普通文件；符号链接和特殊文件会被跳过。
        </p>
        <p v-if="importing" class="haven-dropzone__hint">正在导入文件，请稍候…</p>
      </div>
    </div>

    <div v-if="importing" class="haven-import-progress" role="status" aria-live="polite">
      <div>
        <p class="haven-import-progress__title">
          正在导入{{ progress?.name ? `：${progress.name}` : "…" }}
        </p>
        <p class="haven-import-progress__meta">
          第 {{ progress?.index || 0 }} / {{ progress?.total || 0 }} 个文件
        </p>
      </div>
      <n-button size="small" quaternary :disabled="cancelRequested" @click="handleCancel">
        {{ cancelRequested ? "正在停止…" : "取消导入" }}
      </n-button>
    </div>

    <div
      v-if="batchExportState.status === 'running'"
      class="haven-import-progress"
      role="status"
      aria-live="polite"
    >
      <div>
        <p class="haven-import-progress__title">正在批量导出…</p>
        <p class="haven-import-progress__meta">
          已完成 {{ batchExportState.processed }} / {{ batchExportState.total }} 个文件（成功
          {{ batchExportState.succeeded }}，失败 {{ batchExportState.failed }}）
        </p>
      </div>
      <n-button size="small" quaternary @click="cancelBatchExport">取消剩余导出</n-button>
    </div>

    <div
      v-if="backupState.status === 'running'"
      class="haven-import-progress"
      role="status"
      aria-live="polite"
    >
      <div>
        <p class="haven-import-progress__title">正在备份保险库…</p>
        <p class="haven-import-progress__meta">
          已复制 {{ backupState.processed }} / {{ backupState.total }} 个加密文件（{{
            formatFileSize(backupState.bytes)
          }}）
        </p>
      </div>
    </div>

    <div v-if="backupResult" class="haven-import-result">
      <p class="haven-import-result__head">
        备份完成：{{ backupResult.fileCount }} 个加密文件，共
        {{ formatFileSize(backupResult.totalBytes) }}
      </p>
      <p class="haven-import-progress__meta haven-backup__path">
        备份位置：{{ backupResult.destination }}
      </p>
      <p class="haven-import-progress__meta">
        备份保留原有加密状态，不包含锁文件与临时目录；原保险库可继续使用。
      </p>
    </div>

    <div
      v-if="restoreState.status === 'running'"
      class="haven-import-progress"
      role="status"
      aria-live="polite"
    >
      <div>
        <p class="haven-import-progress__title">正在恢复保险库…</p>
        <p class="haven-import-progress__meta">
          已复制 {{ restoreState.processed }} / {{ restoreState.total }} 个加密文件（{{
            formatFileSize(restoreState.bytes)
          }}）
        </p>
      </div>
    </div>

    <div v-if="restoreResult" class="haven-import-result">
      <p class="haven-import-result__head">
        恢复完成：{{ restoreResult.fileCount }} 个加密文件，共
        {{ formatFileSize(restoreResult.totalBytes) }}
      </p>
      <p class="haven-import-progress__meta haven-backup__path">
        恢复位置：{{ restoreResult.destination }}
      </p>
      <p class="haven-import-progress__meta">
        已保留原有文件 ID、名称、文件夹、收藏与密文；原备份未改变。可锁定后通过「打开已有保险库」打开，并使用该备份所属保险库的主密码解锁。
      </p>
    </div>

    <div v-if="batchExportResult" class="haven-import-result">
      <p class="haven-import-result__head">
        本次导出：成功 {{ batchExportResult.succeeded }} 个<span
          v-if="batchExportResult.failures.length"
        >
          ，失败 {{ batchExportResult.failures.length }} 个</span
        ><span v-if="batchExportResult.cancelled">（已取消剩余任务）</span
        ><span v-if="batchExportResult.interrupted">（已停止）</span>
      </p>
      <p class="haven-import-progress__meta">已导出的文件会保留在目标目录。</p>
      <ul v-if="batchExportResult.failures.length" class="haven-failure-list">
        <li v-for="(item, index) in batchExportResult.failures" :key="`export-fail-${index}`">
          {{ item.name }}：{{ item.message }}
        </li>
      </ul>
    </div>

    <div v-if="summary" class="haven-import-result">
      <p class="haven-import-result__head">
        本次导入：成功 {{ summary.succeeded.length }} 个<span
          v-if="summary.failed.length"
        >
          ，失败 {{ summary.failed.length }} 个</span
        ><span v-if="summary.cancelled">（已取消）</span>
      </p>
      <ul v-if="summary.failed.length" class="haven-failure-list">
        <li v-for="(item, index) in summary.failed" :key="`fail-${index}`">
          {{ item.name }}：{{ item.message }}
        </li>
      </ul>
    </div>

    <div v-if="verifyReport" class="haven-import-result">
      <p class="haven-import-result__head">
        完整性校验：{{ verifyReport.checked }} 个文件，{{ verifyReport.ok.length }} 个完整<span
          v-if="verifyReport.failed.length"
        >
          ，{{ verifyReport.failed.length }} 个异常</span
        ><span v-if="verifyReport.cancelled">（已取消）</span>
      </p>
      <ul v-if="verifyReport.failed.length" class="haven-failure-list">
        <li v-for="item in verifyReport.failed" :key="`verify-${item.id}`">
          {{ item.name }}：{{ item.message }}
        </li>
      </ul>
    </div>

    <div v-if="listState.loading && files.length === 0" class="haven-empty">
      <p class="haven-empty__title">正在读取保险库…</p>
    </div>

    <div
      v-else-if="listState.error && listState.error.code !== 'NOT_UNLOCKED' && files.length === 0"
      class="haven-empty"
    >
      <p class="haven-empty__title">文件列表加载失败</p>
      <p class="haven-empty__text">{{ listState.error.message }}</p>
      <n-button size="small" quaternary :loading="listState.loading" @click="refresh()"
        >重试</n-button
      >
    </div>

    <div v-else-if="files.length === 0" class="haven-empty">
      <p class="haven-empty__title">保险库里还没有文件</p>
      <p class="haven-empty__text">
        点击「导入文件」选择文件，或用「导入文件夹」递归导入其中的普通文件。
        <br />
        原文件会保留在原来的位置，不会被自动删除。
      </p>
    </div>

    <template v-else>
      <p
        v-if="listState.error && listState.error.code !== 'NOT_UNLOCKED'"
        class="haven-list-warning"
      >
        {{ listState.error.message }}
        <button type="button" class="haven-link-button" @click="refresh()">重试</button>
      </p>

      <div class="haven-folders" aria-label="文件夹">
        <div class="haven-folders__list">
          <button
            type="button"
            class="haven-folder haven-folder--all"
            :class="{ 'is-active': activeFolderId === null }"
            :aria-pressed="activeFolderId === null"
            @click="selectFolder(null)"
          >
            <span class="haven-folder__name">全部文件</span>
            <span class="haven-folder__count">{{ browseState.total }}</span>
          </button>
          <div
            v-for="folder in folders"
            :key="folder.id"
            class="haven-folder"
            :class="{ 'is-active': activeFolderId === folder.id }"
          >
            <button
              type="button"
              class="haven-folder__open"
              :aria-pressed="activeFolderId === folder.id"
              @click="selectFolder(folder.id)"
            >
              <span class="haven-folder__name" :title="folder.name">{{ folder.name }}</span>
              <span class="haven-folder__count">{{ folderCounts[folder.id] || 0 }}</span>
            </button>
            <button
              type="button"
              class="haven-folder__action"
              :disabled="
                folderActionBusy ||
                folderDialog.busy ||
                moveState.busy ||
                batchExportState.status === 'running'
              "
              :aria-label="`导出文件夹 ${folder.name}`"
              @click="handleFolderExport(folder)"
            >
              导出
            </button>
            <button
              type="button"
              class="haven-folder__action"
              :disabled="folderActionBusy || folderDialog.busy"
              :aria-label="`重命名文件夹 ${folder.name}`"
              @click="openRenameFolder(folder)"
            >
              改名
            </button>
            <button
              type="button"
              class="haven-folder__action haven-folder__action--danger"
              :disabled="folderActionBusy || folderDialog.busy"
              :aria-label="`删除文件夹 ${folder.name}`"
              @click="handleDeleteFolder(folder)"
            >
              删除
            </button>
          </div>
          <button
            type="button"
            class="haven-folder haven-folder--create"
            aria-label="新建文件夹"
            @click="openCreateFolder"
          >
            <span class="haven-folder__name">+ 新建文件夹</span>
          </button>
        </div>
      </div>

      <div class="haven-toolbar">
        <n-input
          class="haven-toolbar__search"
          :value="browseState.query.keyword"
          clearable
          placeholder="搜索文件名"
          :input-props="{ id: 'haven-search', 'aria-label': '搜索文件名' }"
          @update:value="handleKeyword"
        />
        <div class="haven-view-toggle" role="group" aria-label="收藏筛选">
          <button
            type="button"
            class="haven-view-toggle__btn"
            :class="{ 'is-active': !browseState.query.favoriteOnly }"
            :aria-pressed="!browseState.query.favoriteOnly"
            @click="setFavoriteFilter(false)"
          >
            全部文件
          </button>
          <button
            type="button"
            class="haven-view-toggle__btn"
            :class="{ 'is-active': browseState.query.favoriteOnly === true }"
            :aria-pressed="browseState.query.favoriteOnly === true"
            @click="setFavoriteFilter(true)"
          >
            已收藏
          </button>
        </div>
        <n-select
          class="haven-toolbar__select"
          :value="browseState.query.type"
          :options="TYPE_OPTIONS"
          aria-label="文件类型"
          @update:value="handleTypeChange"
        />
        <n-select
          class="haven-toolbar__select haven-toolbar__select--sort"
          :value="sortValue"
          :options="SORT_OPTIONS"
          aria-label="排序方式"
          @update:value="handleSortChange"
        />
        <div class="haven-view-toggle" role="group" aria-label="视图方式">
          <button
            type="button"
            class="haven-view-toggle__btn"
            :class="{ 'is-active': viewMode === 'list' }"
            :aria-pressed="viewMode === 'list'"
            @click="setViewMode('list')"
          >
            列表
          </button>
          <button
            type="button"
            class="haven-view-toggle__btn"
            :class="{ 'is-active': viewMode === 'grid' }"
            :aria-pressed="viewMode === 'grid'"
            @click="setViewMode('grid')"
          >
            网格
          </button>
        </div>
      </div>

      <div class="haven-list-status">
        <p class="haven-result-count" aria-live="polite">
          全部文件 {{ browseState.total }} 个，当前筛选出
          {{ browseState.matched.length }} 个。
        </p>

        <div v-if="browseState.visible.length > 0" class="haven-selection">
        <n-button
          size="tiny"
          quaternary
          :disabled="batchState.busy || batchExportState.status === 'running'"
          @click="handleToggleSelectAll"
        >
          {{ allVisibleSelected ? "取消全选" : "全选当前列表" }}
        </n-button>
        <span class="haven-selection__count" aria-live="polite">
          {{ selectionState.count > 0 ? `已选择 ${selectionState.count} 个文件` : "未选择文件" }}
        </span>
        <n-button
          v-if="selectionState.count > 0"
          size="tiny"
          type="primary"
          secondary
          :disabled="
            batchState.busy ||
            batchExportState.status === 'running' ||
            deleteState.busy ||
            renameState.busy ||
            importing ||
            exportingId !== null
          "
          @click="handleBatchExport"
        >
          批量导出
        </n-button>
        <n-button
          v-if="selectionState.count > 0"
          size="tiny"
          secondary
          :disabled="
            batchState.busy ||
            batchExportState.status === 'running' ||
            deleteState.busy ||
            renameState.busy ||
            moveState.busy ||
            folderDialog.busy
          "
          @click="requestBatchMove"
        >
          移动到文件夹
        </n-button>
        <n-button
          v-if="selectionState.count > 0"
          size="tiny"
          type="error"
          secondary
          :disabled="
            batchState.busy || batchExportState.status === 'running' || deleteState.busy || renameState.busy
          "
          @click="requestBatchDelete"
        >
          批量删除
        </n-button>
        </div>
      </div>

      <div v-if="browseState.matched.length === 0" class="haven-empty haven-empty--compact">
        <p class="haven-empty__title">
          <template v-if="trimmedKeyword">没有找到包含「{{ trimmedKeyword }}」的文件</template>
          <template v-else-if="activeFolder">文件夹「{{ activeFolder.name }}」中还没有文件</template>
          <template v-else>当前分类下还没有文件</template>
        </p>
        <p class="haven-empty__text">可以清除搜索和筛选条件，查看全部文件。</p>
        <n-button size="small" quaternary @click="handleClearFilters">清除筛选</n-button>
        <n-button v-if="activeFolder" size="small" quaternary @click="selectFolder(null)">
          返回全部文件
        </n-button>
      </div>

      <template v-else>
        <ul v-if="viewMode === 'list'" class="haven-files">
          <li
            v-for="file in browseState.visible"
            :key="file.id"
            class="haven-file"
            :class="{ 'haven-file--new': highlighted.has(file.id) }"
          >
            <span class="haven-file__select">
              <n-checkbox
                :checked="selectionState.idSet.has(file.id)"
                :disabled="batchState.busy || batchExportState.status === 'running'"
                :aria-label="`选择 ${file.name}`"
                @update:checked="(checked) => handleToggleSelect(file, checked)"
              />
            </span>
            <button type="button" class="haven-file__button" @click="handleFileClick(file)">
              <span class="haven-file__type">{{ fileTypeLabel(file.name) }}</span>
              <span class="haven-file__name" :title="file.name">{{ file.name }}</span>
              <span class="haven-file__meta">
                {{ formatFileSize(file.size) }} · {{ formatFileTime(file.importedAt) }}
              </span>
            </button>
            <span class="haven-file__actions">
              <button
                type="button"
                class="haven-file__move"
                :disabled="
                  moveState.busy ||
                  folderDialog.busy ||
                  batchState.busy ||
                  batchExportState.status === 'running'
                "
                :aria-label="`移动 ${file.name}`"
                @click="requestMove(file)"
              >
                移动
              </button>
              <button
                type="button"
                class="haven-favorite-btn haven-file__favorite"
                :class="{ 'is-active': file.favorite === true }"
                :disabled="favoriteBusy.has(file.id)"
                :aria-pressed="file.favorite === true"
                :aria-label="file.favorite === true ? `取消收藏 ${file.name}` : `收藏 ${file.name}`"
                @click="handleToggleFavorite(file)"
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5">
                  <path d="m12 3.6 2.5 5.1 5.6.8-4 3.9.9 5.6-5-2.7-5 2.7.9-5.6-4-3.9 5.6-.8Z" />
                </svg>
              </button>
              <button
                type="button"
                class="haven-file__export"
                :disabled="
                  exportingId !== null ||
                  batchState.busy ||
                  batchExportState.status === 'running'
                "
                :aria-label="`导出 ${file.name}`"
                @click="handleExport(file)"
              >
                {{ exportingId === file.id ? "导出中…" : "导出" }}
              </button>
              <button
                type="button"
                class="haven-file__details"
                :aria-label="`查看 ${file.name} 的详情`"
                @click="handleDetailsClick(file)"
              >
                详情
              </button>
              <button
                type="button"
                class="haven-file__rename"
                :disabled="
                  renameState.busy || batchState.busy || batchExportState.status === 'running'
                "
                :aria-label="`重命名 ${file.name}`"
                @click="requestRename(file)"
              >
                重命名
              </button>
              <button
                type="button"
                class="haven-file__delete"
                :disabled="
                  deleteState.busy || batchState.busy || batchExportState.status === 'running'
                "
                :aria-label="`删除 ${file.name}`"
                @click="requestDelete(file)"
              >
                {{ deleteState.busy && deleteState.target?.id === file.id ? "删除中…" : "删除" }}
              </button>
            </span>
          </li>
        </ul>

        <div v-else class="haven-grid">
          <article
            v-for="file in browseState.visible"
            :key="file.id"
            class="haven-grid-card"
            :class="{ 'haven-grid-card--new': highlighted.has(file.id) }"
          >
            <div class="haven-grid-card__media">
              <button
                type="button"
                class="haven-grid-card__open"
                :aria-label="`预览 ${file.name}`"
                @click="handleFileClick(file)"
              >
                <img
                  v-if="isImageFile(file) && thumbnailState[file.id]?.status === 'ready'"
                  class="haven-grid-card__image"
                  :src="thumbnailState[file.id].url"
                  :alt="file.name"
                />
                <span v-else class="haven-grid-card__placeholder">
                  <svg
                    v-if="isImageFile(file)"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.4"
                    aria-hidden="true"
                  >
                    <rect x="3" y="4" width="18" height="16" rx="2" />
                    <circle cx="9" cy="10" r="1.6" />
                    <path d="m4 18 5-5 4 4 3-3 4 4" />
                  </svg>
                  <svg
                    v-else-if="isVideoName(file.name)"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.4"
                    aria-hidden="true"
                  >
                    <rect x="3" y="5" width="18" height="14" rx="2" />
                    <path d="m10 9 5 3-5 3V9Z" />
                  </svg>
                  <svg
                    v-else
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.4"
                    aria-hidden="true"
                  >
                    <path d="M7 3h7l4 4v14a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1Z" />
                    <path d="M14 3v5h4" />
                  </svg>
                  <span class="haven-grid-card__kind">{{ fileTypeLabel(file.name) }}</span>
                </span>
              </button>
              <span class="haven-grid-card__select">
                <n-checkbox
                  :checked="selectionState.idSet.has(file.id)"
                  :disabled="batchState.busy || batchExportState.status === 'running'"
                  :aria-label="`选择 ${file.name}`"
                  @update:checked="(checked) => handleToggleSelect(file, checked)"
                />
              </span>
              <button
                type="button"
                class="haven-favorite-btn haven-grid-card__favorite"
                :class="{ 'is-active': file.favorite === true }"
                :disabled="favoriteBusy.has(file.id)"
                :aria-pressed="file.favorite === true"
                :aria-label="file.favorite === true ? `取消收藏 ${file.name}` : `收藏 ${file.name}`"
                @click="handleToggleFavorite(file)"
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5">
                  <path d="m12 3.6 2.5 5.1 5.6.8-4 3.9.9 5.6-5-2.7-5 2.7.9-5.6-4-3.9 5.6-.8Z" />
                </svg>
              </button>
            </div>
            <div class="haven-grid-card__body">
              <p class="haven-grid-card__name" :title="file.name">{{ file.name }}</p>
              <p class="haven-grid-card__meta">
                {{ fileTypeLabel(file.name) }} · {{ formatFileSize(file.size) }}
              </p>
            </div>
            <div class="haven-grid-card__actions">
              <button
                type="button"
                class="haven-grid-card__action"
                :disabled="
                  moveState.busy ||
                  folderDialog.busy ||
                  batchState.busy ||
                  batchExportState.status === 'running'
                "
                :aria-label="`移动 ${file.name}`"
                @click="requestMove(file)"
              >
                移动
              </button>
              <button
                type="button"
                class="haven-grid-card__action"
                :disabled="
                  exportingId !== null ||
                  batchState.busy ||
                  batchExportState.status === 'running'
                "
                :aria-label="`导出 ${file.name}`"
                @click="handleExport(file)"
              >
                {{ exportingId === file.id ? "导出中…" : "导出" }}
              </button>
              <button
                type="button"
                class="haven-grid-card__action"
                :aria-label="`查看 ${file.name} 的详情`"
                @click="handleDetailsClick(file)"
              >
                详情
              </button>
              <button
                type="button"
                class="haven-grid-card__action"
                :disabled="
                  renameState.busy || batchState.busy || batchExportState.status === 'running'
                "
                :aria-label="`重命名 ${file.name}`"
                @click="requestRename(file)"
              >
                重命名
              </button>
              <button
                type="button"
                class="haven-grid-card__action haven-grid-card__action--danger"
                :disabled="
                  deleteState.busy || batchState.busy || batchExportState.status === 'running'
                "
                :aria-label="`删除 ${file.name}`"
                @click="requestDelete(file)"
              >
                {{ deleteState.busy && deleteState.target?.id === file.id ? "删除中…" : "删除" }}
              </button>
            </div>
          </article>
        </div>

        <div v-if="browseState.hiddenCount > 0" class="haven-load-more">
          <n-button size="small" quaternary @click="showMore">
            显示更多（还有 {{ browseState.hiddenCount }} 个）
          </n-button>
        </div>
      </template>
    </template>

    <div class="haven-actions haven-actions--start">
      <div class="haven-actions__row">
        <n-button
          type="primary"
          size="large"
          :loading="importing"
          title="导入会逐块加密写入保险库；原文件不会被删除。"
          @click="handleImport"
        >
          导入文件
        </n-button>
        <n-button
          size="large"
          secondary
          :loading="importing"
          :disabled="importing"
          @click="handleImportFolders"
        >
          导入文件夹
        </n-button>
        <n-button
          quaternary
          size="large"
          :loading="verifying"
          :disabled="files.length === 0 || importing"
          @click="handleVerify"
        >
          校验完整性
        </n-button>
        <n-button
          quaternary
          size="large"
          :loading="backupState.status === 'running'"
          :disabled="backupState.status === 'running' || importing"
          @click="handleBackupVault"
        >
          备份保险库
        </n-button>
        <n-button
          quaternary
          size="large"
          :loading="restoreState.status === 'running'"
          :disabled="restoreState.status === 'running' || backupState.status === 'running' || importing"
          @click="handleRestoreVault"
        >
          恢复保险库
        </n-button>
        <n-button quaternary size="large" @click="handleLock">锁定保险库</n-button>
      </div>
    </div>

    <Teleport to="body">
      <div
        v-if="previewState.status !== 'idle'"
        class="haven-preview"
        role="dialog"
        aria-modal="true"
        aria-label="图片预览"
        @click.self="closePreview"
      >
        <div class="haven-preview__panel">
          <div class="haven-preview__head">
            <p class="haven-preview__name" :title="previewState.name">
              {{ previewState.name }}
            </p>
            <n-button quaternary size="small" @click="closePreview">关闭</n-button>
          </div>
          <div class="haven-preview__body">
            <p v-if="previewState.status === 'loading'" class="haven-preview__hint">
              正在解密图片…
            </p>
            <p
              v-else-if="previewState.status === 'error'"
              class="haven-preview__hint haven-preview__hint--error"
            >
              {{ previewState.message }}
            </p>
            <img
              v-else-if="previewState.status === 'ready'"
              :src="previewState.url"
              :alt="previewState.name"
              class="haven-preview__image"
            />
          </div>
          <p class="haven-preview__foot">
            关闭预览会撤销图片地址；内存何时回收由系统决定。
          </p>
        </div>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="pdfState.status !== 'idle'"
        class="haven-preview"
        role="dialog"
        aria-modal="true"
        aria-label="PDF 预览"
        @click.self="closePdfPreview"
      >
        <div class="haven-preview__panel haven-preview__panel--pdf">
          <div class="haven-preview__head">
            <p class="haven-preview__name" :title="pdfState.name">
              {{ pdfState.name }}
            </p>
            <n-button quaternary size="small" @click="closePdfPreview">关闭</n-button>
          </div>
          <div class="haven-preview__body haven-preview__body--pdf">
            <p v-if="pdfState.status === 'loading'" class="haven-preview__hint">
              正在解密 PDF…
            </p>
            <p
              v-else-if="pdfState.status === 'error'"
              class="haven-preview__hint haven-preview__hint--error"
              role="alert"
            >
              {{ pdfState.message }}
            </p>
            <PdfPreview v-else-if="pdfState.status === 'ready'" :bytes="pdfState.bytes" />
          </div>
          <p class="haven-preview__foot">
            PDF 在内存中解密与渲染，不会写入临时文件；关闭预览会释放渲染资源。
          </p>
        </div>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="videoState.status !== 'idle'"
        class="haven-preview"
        role="dialog"
        aria-modal="true"
        aria-label="视频预览"
        @click.self="closeVideoPreview"
      >
        <div class="haven-preview__panel haven-preview__panel--video">
          <div class="haven-preview__head">
            <p class="haven-preview__name" :title="videoState.name">
              {{ videoState.name }}
            </p>
            <n-button quaternary size="small" @click="closeVideoPreview">关闭</n-button>
          </div>
          <div class="haven-preview__body haven-preview__body--video">
            <p v-if="videoState.status === 'loading'" class="haven-preview__hint">
              正在解密视频…
            </p>
            <p
              v-else-if="videoState.status === 'error'"
              class="haven-preview__hint haven-preview__hint--error"
              role="alert"
            >
              {{ videoState.message }}
            </p>
            <VideoPreview
              v-else-if="videoState.status === 'ready'"
              :url="videoState.url"
              :name="videoState.name"
            />
          </div>
          <p class="haven-preview__foot">
            视频在内存中解密并由系统播放器组件播放，不会写入临时文件；关闭预览会释放播放资源。
          </p>
        </div>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="restoreDialog.open"
        class="haven-delete"
        role="dialog"
        aria-modal="true"
        aria-labelledby="haven-restore-title"
        :aria-busy="restoreDialog.busy"
        @click.self="closeRestoreDialog"
      >
        <div class="haven-delete__panel">
          <p class="haven-eyebrow">恢复保险库</p>
          <h2 id="haven-restore-title" class="haven-delete__title">
            输入该备份的主密码
          </h2>
          <n-input
            class="haven-rename__field"
            type="password"
            :value="restoreDialog.password"
            :disabled="restoreDialog.busy"
            show-password-on="mousedown"
            placeholder="创建该备份的保险库的主密码"
            :input-props="{
              'aria-label': '备份主密码',
              onKeydown: handleRestoreDialogKeydown,
            }"
            @update:value="handleRestorePasswordDraft"
          />
          <p class="haven-rename__hint">
            验证只读取备份中的加密索引，不会解密任何文件；通过后再选择恢复位置。
          </p>
          <p v-if="restoreDialog.error" class="haven-rename__error" role="alert">
            {{ restoreDialog.error.message }}
          </p>
          <p v-if="restoreDialog.busy" class="haven-delete__status" role="status">
            正在验证主密码…请稍候。
          </p>
          <div class="haven-delete__actions">
            <n-button quaternary size="small" @click="handleLock">锁定保险库</n-button>
            <div class="haven-delete__confirm-actions">
              <n-button
                size="small"
                :disabled="restoreDialog.busy"
                @click="closeRestoreDialog"
              >
                取消
              </n-button>
              <n-button
                type="primary"
                size="small"
                :loading="restoreDialog.busy"
                :disabled="restoreDialog.busy"
                @click="confirmRestorePassword"
              >
                验证密码
              </n-button>
            </div>
          </div>
        </div>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="folderDialog.open"
        class="haven-delete"
        role="dialog"
        aria-modal="true"
        aria-labelledby="haven-folder-title"
        :aria-busy="folderDialog.busy"
        @click.self="closeFolderDialog"
      >
        <div class="haven-delete__panel">
          <p class="haven-eyebrow">
            {{ folderDialog.mode === "create" ? "新建文件夹" : "重命名文件夹" }}
          </p>
          <h2 id="haven-folder-title" class="haven-delete__title">
            <template v-if="folderDialog.mode === 'create'">为文件创建新的文件夹</template>
            <template v-else>修改「{{ folderDialog.originalName }}」的名称</template>
          </h2>
          <n-input
            class="haven-rename__field"
            :value="folderDialog.draft"
            autofocus
            placeholder="输入文件夹名称"
            :input-props="{
              'aria-label': '文件夹名称',
              onKeydown: handleFolderKeydown,
            }"
            @update:value="handleFolderDraft"
          />
          <p class="haven-rename__hint">
            文件夹名称最多 100 个字符，不能包含 / \ : 等非法字符；只建立一级文件夹。
          </p>
          <p v-if="folderDialog.error" class="haven-rename__error" role="alert">
            {{ folderDialog.error.message }}
          </p>
          <p v-if="folderDialog.busy" class="haven-delete__status" role="status">
            正在保存…请等待操作完成；锁定入口仍可使用。
          </p>
          <div class="haven-delete__actions">
            <n-button quaternary size="small" @click="handleLock">锁定保险库</n-button>
            <div class="haven-delete__confirm-actions">
              <n-button size="small" :disabled="folderDialog.busy" @click="closeFolderDialog">
                取消
              </n-button>
              <n-button
                type="primary"
                size="small"
                :loading="folderDialog.busy"
                :disabled="folderDialog.busy"
                @click="confirmFolderDialog"
              >
                保存
              </n-button>
            </div>
          </div>
        </div>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="moveState.open"
        class="haven-delete"
        role="dialog"
        aria-modal="true"
        aria-labelledby="haven-move-title"
        :aria-busy="moveState.busy"
        @click.self="cancelMove"
      >
        <div class="haven-delete__panel">
          <p class="haven-eyebrow">移动到文件夹</p>
          <h2 id="haven-move-title" class="haven-delete__title">
            移动 {{ moveState.ids.length }} 个文件
          </h2>
          <p class="haven-rename__hint">
            <template v-if="moveState.names.length === 1">「{{ moveState.names[0] }}」</template>
            <template v-else>已选择的 {{ moveState.ids.length }} 个文件</template>
            将移动到所选位置；只更新索引中的归属关系，不会重新加密文件。
          </p>
          <n-select
            class="haven-move__select"
            :value="moveState.target"
            :options="moveOptions"
            aria-label="目标文件夹"
            @update:value="handleMoveTarget"
          />
          <p v-if="moveState.busy" class="haven-delete__status" role="status">
            正在移动…请等待操作完成；锁定入口仍可使用。
          </p>
          <div class="haven-delete__actions">
            <n-button quaternary size="small" @click="handleLock">锁定保险库</n-button>
            <div class="haven-delete__confirm-actions">
              <n-button size="small" :disabled="moveState.busy" @click="cancelMove">
                取消
              </n-button>
              <n-button
                type="primary"
                size="small"
                :loading="moveState.busy"
                :disabled="moveState.busy"
                @click="confirmMove"
              >
                确认移动
              </n-button>
            </div>
          </div>
        </div>
      </div>
    </Teleport>

    <FileDetailsDialog
      v-if="detailsState"
      :file="detailsState"
      @close="closeDetails"
    />

    <Teleport to="body">
      <div
        v-if="batchState.open"
        class="haven-delete"
        role="dialog"
        aria-modal="true"
        aria-labelledby="haven-batch-title"
        aria-describedby="haven-batch-description"
        :aria-busy="batchState.busy"
        @click.self="cancelBatchDelete"
      >
        <div class="haven-delete__panel">
          <p class="haven-eyebrow">批量删除</p>
          <h2 id="haven-batch-title" class="haven-delete__title">
            从 Haven 移除 {{ batchState.total }} 个文件？
          </h2>
          <div id="haven-batch-description" class="haven-delete__description">
            <p>将删除已选择的 {{ batchState.total }} 个文件，删除后无法通过 Haven 恢复。</p>
            <p>导入前的原始文件及此前导出的副本不受影响。</p>
            <p>
              普通删除不等于磁盘安全擦除，也不会清除系统快照、备份或其他历史副本。
            </p>
          </div>
          <p v-if="batchState.busy" class="haven-delete__status" role="status">
            正在删除 {{ batchState.processed }} / {{ batchState.total }} 个文件，无法取消。请等待操作完成；锁定入口仍可使用。
          </p>
          <div class="haven-delete__actions">
            <n-button quaternary size="small" @click="handleLock">锁定保险库</n-button>
            <div class="haven-delete__confirm-actions">
              <n-button size="small" :disabled="batchState.busy" @click="cancelBatchDelete">
                取消
              </n-button>
              <n-button
                type="error"
                size="small"
                :loading="batchState.busy"
                :disabled="batchState.busy"
                @click="confirmBatchDelete"
              >
                确认删除 {{ batchState.total }} 个
              </n-button>
            </div>
          </div>
        </div>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="renameState.target"
        class="haven-delete"
        role="dialog"
        aria-modal="true"
        aria-labelledby="haven-rename-title"
        :aria-busy="renameState.busy"
        @click.self="cancelRename"
      >
        <div class="haven-delete__panel">
          <p class="haven-eyebrow">重命名文件</p>
          <h2 id="haven-rename-title" class="haven-delete__title">
            修改「{{ renameState.target.name }}」的名称
          </h2>
          <n-input
            class="haven-rename__field"
            :value="renameState.draft"
            autofocus
            placeholder="输入新的文件名"
            :input-props="{
              'aria-label': '新的文件名',
              onKeydown: handleRenameKeydown,
              onFocus: handleRenameFocus,
            }"
            @update:value="handleRenameInput"
          />
          <p class="haven-rename__hint">
            只修改显示名称，文件内容与文件 ID 保持不变；扩展名需保留，最多
            {{ FILE_NAME_MAX_LENGTH }} 个字符。
          </p>
          <p v-if="renameState.error" class="haven-rename__error" role="alert">
            {{ renameState.error.message }}
          </p>
          <p v-if="renameState.busy" class="haven-delete__status" role="status">
            正在保存…请等待操作完成；锁定入口仍可使用。
          </p>
          <div class="haven-delete__actions">
            <n-button quaternary size="small" @click="handleLock">锁定保险库</n-button>
            <div class="haven-delete__confirm-actions">
              <n-button size="small" :disabled="renameState.busy" @click="cancelRename">
                取消
              </n-button>
              <n-button
                type="primary"
                size="small"
                :loading="renameState.busy"
                :disabled="renameState.busy"
                @click="confirmRename"
              >
                保存
              </n-button>
            </div>
          </div>
        </div>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="deleteState.target"
        class="haven-delete"
        role="dialog"
        aria-modal="true"
        aria-labelledby="haven-delete-title"
        aria-describedby="haven-delete-description"
        :aria-busy="deleteState.busy"
        @click.self="cancelDelete"
      >
        <div class="haven-delete__panel">
          <p class="haven-eyebrow">单文件删除</p>
          <h2 id="haven-delete-title" class="haven-delete__title">
            从 Haven 移除「{{ deleteState.target.name }}」？
          </h2>
          <div id="haven-delete-description" class="haven-delete__description">
            <p>删除后无法通过 Haven 恢复该文件。</p>
            <p>导入前的原始文件及此前导出的副本不受影响。</p>
            <p>
              普通删除不等于磁盘安全擦除，也不会清除系统快照、备份或其他历史副本。
            </p>
          </div>
          <p v-if="deleteState.busy" class="haven-delete__status" role="status">
            正在删除，无法取消。请等待操作完成；锁定入口仍可使用。
          </p>
          <div class="haven-delete__actions">
            <n-button quaternary size="small" @click="handleLock">锁定保险库</n-button>
            <div class="haven-delete__confirm-actions">
              <n-button size="small" :disabled="deleteState.busy" @click="cancelDelete">
                取消
              </n-button>
              <n-button
                type="error"
                size="small"
                :loading="deleteState.busy"
                :disabled="deleteState.busy"
                @click="confirmDelete"
              >
                确认删除
              </n-button>
            </div>
          </div>
        </div>
      </div>
    </Teleport>
  </section>
</template>
