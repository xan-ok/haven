<script setup>
import { computed } from "vue";
import { NButton } from "naive-ui";
import { findTheme, previewTokens } from "../theme/manager.js";
import { cssVariables } from "../theme/tokens.js";

const props = defineProps({
  show: { type: Boolean, default: false },
  themeId: { type: String, default: "" },
});
const emit = defineEmits(["update:show", "apply"]);

const theme = computed(() => findTheme(props.themeId));
const tokens = computed(() => (theme.value ? previewTokens(theme.value.id) : null));
const previewStyle = computed(() => {
  if (!tokens.value) return {};
  return {
    ...cssVariables(tokens.value),
    background: `linear-gradient(168deg, ${tokens.value.bgTop} 0%, ${tokens.value.bgBottom} 100%)`,
  };
});

function close() {
  emit("update:show", false);
}

function handleApply() {
  if (!theme.value) return;
  emit("apply", theme.value.id);
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="show && theme"
      class="theme-overlay theme-overlay--preview"
      @click.self="close"
    >
      <div
        class="theme-preview-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="theme-preview-title"
      >
        <header class="theme-preview-panel__head">
          <div>
            <p class="theme-preview-panel__eyebrow">主题预览</p>
            <h3 id="theme-preview-title" class="theme-preview-panel__title">
              {{ theme.name }}
            </h3>
            <p class="theme-preview-panel__desc">
              {{ theme.description || "自定义 Haven 主题" }}
            </p>
          </div>
          <button
            type="button"
            class="theme-center__close"
            aria-label="关闭预览"
            @click="close"
          >
            ×
          </button>
        </header>

        <div class="theme-preview" :style="previewStyle">
          <div class="theme-preview__window">
            <div class="theme-preview__topbar">
              <span class="theme-preview__mark">Haven</span>
              <span class="theme-preview__status">已解锁</span>
            </div>

            <div class="theme-preview__content">
              <div class="theme-preview__card">
                <p class="theme-preview__label">保险库</p>
                <h4 class="theme-preview__title">我的保险库</h4>
                <p class="theme-preview__text">你的文件，只属于你。</p>

                <div class="theme-preview__field">
                  <span class="theme-preview__field-label">搜索文件</span>
                  <div class="theme-preview__input">输入关键词…</div>
                </div>

                <div class="theme-preview__actions">
                  <button type="button" class="theme-preview__btn theme-preview__btn--primary">
                    导入文件
                  </button>
                  <button type="button" class="theme-preview__btn">备份保险库</button>
                </div>
              </div>

              <div class="theme-preview__list" aria-hidden="true">
                <div class="theme-preview__row">
                  <span class="theme-preview__thumb"></span>
                  <span class="theme-preview__row-name">海边日落.jpg</span>
                  <span class="theme-preview__meta">2.4 MB</span>
                </div>
                <div class="theme-preview__row is-selected">
                  <span class="theme-preview__thumb"></span>
                  <span class="theme-preview__row-name">旅行清单.pdf</span>
                  <span class="theme-preview__meta">已选中</span>
                </div>
                <div class="theme-preview__row">
                  <span class="theme-preview__thumb"></span>
                  <span class="theme-preview__row-name">手写笔记.png</span>
                  <span class="theme-preview__meta">820 KB</span>
                </div>
              </div>

              <div class="theme-preview__alerts">
                <p class="theme-preview__alert theme-preview__alert--success">
                  导入完成，共 3 个文件。
                </p>
                <p class="theme-preview__alert theme-preview__alert--warning">
                  部分文件已跳过，请检查后重试。
                </p>
                <p class="theme-preview__alert theme-preview__alert--danger">
                  无法读取该文件，请确认文件是否损坏。
                </p>
              </div>

              <div class="theme-preview__dialog" aria-hidden="true">
                <p class="theme-preview__dialog-title">删除这个文件？</p>
                <p class="theme-preview__dialog-text">删除后无法恢复，请确认已备份。</p>
                <div class="theme-preview__dialog-actions">
                  <button type="button" class="theme-preview__btn theme-preview__btn--primary">
                    删除
                  </button>
                  <button type="button" class="theme-preview__btn">取消</button>
                </div>
              </div>
            </div>
          </div>
        </div>

        <footer class="theme-preview-panel__foot">
          <p class="theme-preview-panel__hint">
            预览只在当前面板内生效，关闭或取消不会改变正在使用的主题。
          </p>
          <div class="theme-preview-panel__actions">
            <n-button @click="close">取消</n-button>
            <n-button type="primary" @click="handleApply">应用主题</n-button>
          </div>
        </footer>
      </div>
    </div>
  </Teleport>
</template>
