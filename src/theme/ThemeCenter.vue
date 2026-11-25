<script setup>
import { onBeforeUnmount, ref, watch } from "vue";
import { NButton, useMessage } from "naive-ui";
import ThemePreview from "./ThemePreview.vue";
import {
  activeTheme,
  applyTheme,
  builtinThemes,
  deleteTheme,
  importThemeFile,
  initTheme,
  refreshUserThemes,
  userThemes,
} from "../theme/manager.js";
import {
  chooseThemeFile,
  isThemeStorageAvailable,
  normalizeThemeError,
} from "../theme/storage.js";
import { buildTokens, cssVariables } from "../theme/tokens.js";

const props = defineProps({
  show: { type: Boolean, default: false },
});
const emit = defineEmits(["update:show"]);

const message = useMessage();

const previewId = ref("");
const deleteTarget = ref(null);
const conflictPath = ref("");
const importing = ref(false);
const errorText = ref("");
const notice = ref("");

function cardStyle(theme) {
  const tokens = buildTokens(theme);
  return {
    ...cssVariables(tokens),
    background: `linear-gradient(160deg, ${tokens.bgTop} 0%, ${tokens.bgBottom} 100%)`,
  };
}

function swatches(theme) {
  return [
    theme.colors.primary,
    theme.colors.background,
    theme.colors.surface,
    theme.colors.text,
    theme.colors.border,
  ];
}

function close() {
  emit("update:show", false);
}

function closeTransient() {
  if (previewId.value) {
    previewId.value = "";
    return true;
  }
  if (deleteTarget.value) {
    deleteTarget.value = null;
    return true;
  }
  if (conflictPath.value) {
    conflictPath.value = "";
    return true;
  }
  return false;
}

function onKeydown(event) {
  if (event.key !== "Escape") return;
  if (closeTransient()) return;
  close();
}

watch(
  () => props.show,
  async (visible) => {
    if (!visible) {
      window.removeEventListener("keydown", onKeydown);
      return;
    }
    window.addEventListener("keydown", onKeydown);
    errorText.value = "";
    notice.value = "";
    previewId.value = "";
    deleteTarget.value = null;
    conflictPath.value = "";
    await initTheme();
    try {
      await refreshUserThemes();
    } catch {
      // 主题目录不可读时保留内置主题，不阻塞主题中心使用。
    }
  }
);

onBeforeUnmount(() => window.removeEventListener("keydown", onKeydown));

async function handleApply(id) {
  try {
    const theme = await applyTheme(id);
    previewId.value = "";
    errorText.value = "";
    notice.value = `已切换到「${theme.name}」。`;
    message.success(`已应用「${theme.name}」主题。`);
  } catch (error) {
    previewId.value = "";
    const normalized = normalizeThemeError(error);
    errorText.value = normalized.message;
    message.error(normalized.message);
  }
}

async function importFile(path, overwrite) {
  importing.value = true;
  errorText.value = "";
  try {
    const theme = await importThemeFile(path, overwrite);
    notice.value = `已导入「${theme.name}」，请先预览再决定是否应用。`;
    message.success(`主题「${theme.name}」已导入。`);
    previewId.value = theme.id;
  } catch (error) {
    const normalized = normalizeThemeError(error);
    if (normalized.code === "THEME_EXISTS") {
      conflictPath.value = path;
      return;
    }
    errorText.value = normalized.message;
    message.error(normalized.message);
  } finally {
    importing.value = false;
  }
}

async function startImport() {
  if (!isThemeStorageAvailable()) {
    message.warning("浏览器预览无法导入主题，请在桌面应用中操作。");
    return;
  }
  try {
    const path = await chooseThemeFile();
    if (!path) return;
    await importFile(path, false);
  } catch (error) {
    const normalized = normalizeThemeError(error);
    errorText.value = normalized.message;
    message.error(normalized.message);
  }
}

async function confirmOverwrite() {
  const path = conflictPath.value;
  conflictPath.value = "";
  if (!path) return;
  await importFile(path, true);
}

function requestDelete(theme) {
  deleteTarget.value = theme;
}

async function confirmDelete() {
  const target = deleteTarget.value;
  deleteTarget.value = null;
  if (!target) return;
  try {
    await deleteTheme(target.id);
    notice.value = `已删除「${target.name}」。`;
    message.success(`主题「${target.name}」已删除。`);
  } catch (error) {
    const normalized = normalizeThemeError(error);
    errorText.value = normalized.message;
    message.error(normalized.message);
  }
}
</script>

<template>
  <Teleport to="body">
    <div v-if="show" class="theme-overlay" @click.self="close">
      <div
        class="theme-center"
        role="dialog"
        aria-modal="true"
        aria-labelledby="theme-center-title"
      >
        <header class="theme-center__head">
          <div class="theme-center__heading">
            <p class="theme-eyebrow">APPEARANCE</p>
            <h2 id="theme-center-title" class="theme-center__title">主题中心</h2>
            <p class="theme-center__desc">
              选择内置风格，或导入自己制作的主题。切换立即生效，无需重启。
            </p>
          </div>
          <div class="theme-center__head-actions">
            <n-button
              size="small"
              type="primary"
              :loading="importing"
              :disabled="!isThemeStorageAvailable()"
              @click="startImport"
            >
              导入主题
            </n-button>
            <button
              type="button"
              class="theme-center__close"
              aria-label="关闭主题中心"
              @click="close"
            >
              ×
            </button>
          </div>
        </header>

        <p v-if="errorText" class="haven-error theme-center__message">{{ errorText }}</p>
        <p v-else-if="notice" class="haven-notice theme-center__message">{{ notice }}</p>

        <div class="theme-center__scroll">
          <section class="theme-center__section" aria-labelledby="builtin-themes-title">
            <div class="theme-center__section-head">
              <h3 id="builtin-themes-title" class="theme-center__section-title">系统主题</h3>
              <span class="theme-center__count">{{ builtinThemes.length }} 种内置风格</span>
            </div>

            <div class="theme-grid">
              <article
                v-for="theme in builtinThemes"
                :key="theme.id"
                class="theme-card"
                :class="{ 'is-active': theme.id === activeTheme.id }"
              >
                <div class="theme-card__preview" :style="cardStyle(theme)">
                  <span class="theme-card__window" aria-hidden="true">
                    <span class="theme-card__bar"></span>
                    <span class="theme-card__mini">
                      <span class="theme-card__line"></span>
                      <span class="theme-card__line theme-card__line--short"></span>
                      <span class="theme-card__pill"></span>
                    </span>
                  </span>
                </div>
                <div class="theme-card__body">
                  <div class="theme-card__title-row">
                    <h4 class="theme-card__name">{{ theme.name }}</h4>
                    <span v-if="theme.id === activeTheme.id" class="theme-card__badge">
                      使用中
                    </span>
                  </div>
                  <p class="theme-card__desc">{{ theme.description }}</p>
                  <div class="theme-card__swatches" aria-hidden="true">
                    <span
                      v-for="color in swatches(theme)"
                      :key="color"
                      class="theme-card__swatch"
                      :style="{ background: color }"
                    ></span>
                  </div>
                  <div class="theme-card__actions">
                    <n-button size="tiny" quaternary @click="previewId = theme.id">
                      预览
                    </n-button>
                    <n-button
                      size="tiny"
                      type="primary"
                      :disabled="theme.id === activeTheme.id"
                      @click="handleApply(theme.id)"
                    >
                      应用
                    </n-button>
                  </div>
                </div>
              </article>
            </div>
          </section>

          <section class="theme-center__section" aria-labelledby="user-themes-title">
            <div class="theme-center__section-head">
              <h3 id="user-themes-title" class="theme-center__section-title">我的主题</h3>
              <span class="theme-center__count">{{ userThemes.length }} 个自定义主题</span>
            </div>

            <div v-if="userThemes.length === 0" class="theme-empty">
              <p class="theme-empty__title">还没有导入主题</p>
              <p class="theme-empty__text">
                主题文件为 .haventheme。导入后可以先预览，再决定是否应用。
              </p>
            </div>

            <div v-else class="theme-grid">
              <article
                v-for="theme in userThemes"
                :key="theme.id"
                class="theme-card"
                :class="{ 'is-active': theme.id === activeTheme.id }"
              >
                <div class="theme-card__preview" :style="cardStyle(theme)">
                  <span class="theme-card__window" aria-hidden="true">
                    <span class="theme-card__bar"></span>
                    <span class="theme-card__mini">
                      <span class="theme-card__line"></span>
                      <span class="theme-card__line theme-card__line--short"></span>
                      <span class="theme-card__pill"></span>
                    </span>
                  </span>
                </div>
                <div class="theme-card__body">
                  <div class="theme-card__title-row">
                    <h4 class="theme-card__name">{{ theme.name }}</h4>
                    <span v-if="theme.id === activeTheme.id" class="theme-card__badge">
                      使用中
                    </span>
                  </div>
                  <p class="theme-card__desc">
                    {{ theme.description || "自定义主题" }}
                  </p>
                  <p class="theme-card__author">作者：{{ theme.author || "未署名" }}</p>
                  <div class="theme-card__swatches" aria-hidden="true">
                    <span
                      v-for="color in swatches(theme)"
                      :key="color"
                      class="theme-card__swatch"
                      :style="{ background: color }"
                    ></span>
                  </div>
                  <div class="theme-card__actions">
                    <n-button size="tiny" quaternary @click="previewId = theme.id">
                      预览
                    </n-button>
                    <n-button
                      size="tiny"
                      type="primary"
                      :disabled="theme.id === activeTheme.id"
                      @click="handleApply(theme.id)"
                    >
                      应用
                    </n-button>
                    <n-button
                      size="tiny"
                      quaternary
                      :disabled="theme.id === activeTheme.id"
                      :title="
                        theme.id === activeTheme.id
                          ? '当前主题需先切换到其他主题再删除'
                          : '删除主题'
                      "
                      @click="requestDelete(theme)"
                    >
                      删除
                    </n-button>
                  </div>
                </div>
              </article>
            </div>
          </section>
        </div>

        <footer class="theme-center__foot">
          主题只保存在本机应用配置目录，与保险库内容完全隔离。
        </footer>
      </div>
    </div>

    <div v-if="deleteTarget" class="theme-overlay theme-overlay--confirm" @click.self="deleteTarget = null">
      <div class="theme-confirm" role="alertdialog" aria-modal="true">
        <h3 class="theme-confirm__title">删除「{{ deleteTarget.name }}」？</h3>
        <p class="theme-confirm__text">
          只会删除本机保存的主题文件，不影响保险库、文件和其他主题。
        </p>
        <div class="theme-confirm__actions">
          <n-button size="small" @click="deleteTarget = null">取消</n-button>
          <n-button size="small" type="error" @click="confirmDelete">删除主题</n-button>
        </div>
      </div>
    </div>

    <div v-if="conflictPath" class="theme-overlay theme-overlay--confirm" @click.self="conflictPath = ''">
      <div class="theme-confirm" role="alertdialog" aria-modal="true">
        <h3 class="theme-confirm__title">已存在相同 ID 的主题</h3>
        <p class="theme-confirm__text">
          导入文件中的主题 ID 与已有自定义主题冲突。可以覆盖原主题，或取消后修改文件中的 id。
        </p>
        <div class="theme-confirm__actions">
          <n-button size="small" @click="conflictPath = ''">取消</n-button>
          <n-button size="small" type="primary" @click="confirmOverwrite">覆盖主题</n-button>
        </div>
      </div>
    </div>

    <ThemePreview
      :show="Boolean(previewId)"
      :theme-id="previewId"
      @update:show="previewId = ''"
      @apply="handleApply"
    />
  </Teleport>
</template>
