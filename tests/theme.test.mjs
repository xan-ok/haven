import test from "node:test";
import assert from "node:assert/strict";
import { BUILTIN_THEMES, DEFAULT_THEME_ID, isBuiltinThemeId } from "../src/theme/presets.js";
import {
  buildTokens,
  contrastText,
  cssVariables,
  createThemeOverrides,
  isHexColor,
  mix,
  normalizeRadius,
  relativeLuminance,
} from "../src/theme/tokens.js";

const REQUIRED_IDS = ["haven-original", "snow", "midnight", "forest", "sand", "lavender"];
const COLOR_KEYS = [
  "primary",
  "background",
  "surface",
  "text",
  "textMuted",
  "border",
  "success",
  "warning",
  "danger",
];

function contrast(a, b) {
  const high = Math.max(relativeLuminance(a), relativeLuminance(b));
  const low = Math.min(relativeLuminance(a), relativeLuminance(b));
  return (high + 0.05) / (low + 0.05);
}

function customTheme(colors, radius = { small: 6, medium: 10, large: 14 }) {
  return { id: "custom", name: "Custom", colors, radius };
}

test("内置主题覆盖 6 种预设且字段完整", () => {
  assert.deepEqual(
    BUILTIN_THEMES.map((theme) => theme.id),
    REQUIRED_IDS
  );
  for (const theme of BUILTIN_THEMES) {
    assert.ok(theme.builtin);
    for (const key of COLOR_KEYS) {
      assert.ok(isHexColor(theme.colors[key]), `${theme.id} 的 ${key} 必须是 #RRGGBB`);
    }
    assert.ok(theme.name.length > 0);
    assert.ok(theme.description.length > 0);
  }
  assert.equal(DEFAULT_THEME_ID, "haven-original");
  assert.ok(isBuiltinThemeId("midnight"));
  assert.equal(isBuiltinThemeId("ocean-demo"), false);
});

test("Haven Original 保留原版视觉变量", () => {
  const tokens = buildTokens(BUILTIN_THEMES[0]);
  assert.equal(tokens.bgTop, "#FBF8F3");
  assert.equal(tokens.bgBottom, "#F1ECE4");
  assert.equal(tokens.ink, "#3A3631");
  assert.equal(tokens.inkSoft, "#6F6860");
  assert.equal(tokens.inkFaint, "#9A9289");
  assert.equal(tokens.accent, "#7E8F7C");
  assert.equal(tokens.surface, "rgba(255, 255, 255, 0.62)");
  assert.equal(tokens.radius.medium, 10);
  assert.equal(cssVariables(tokens)["--haven-radius-card"], "22px");
});

test("自定义浅色主题的派生颜色保持可读对比度", () => {
  const tokens = buildTokens(
    customTheme({
      primary: "#4A90A4",
      background: "#F2F7F8",
      surface: "#FFFFFF",
      text: "#24343A",
      textMuted: "#71858C",
      border: "#DCE7EA",
      success: "#438A70",
      warning: "#C28B45",
      danger: "#C65B5B",
    })
  );
  assert.equal(tokens.dark, false);
  assert.ok(contrast(tokens.ink, tokens.bgTop) >= 7);
  assert.ok(contrast(tokens.inkSoft, tokens.bgTop) >= 3.5);
  assert.ok(contrast(tokens.inkFaint, tokens.bgTop) >= 3);
  assert.equal(tokens.accentContrast, "#171512");
  assert.match(tokens.surface, /^rgba\(255, 255, 255, 0\.74\)$/);
});

test("自定义深色主题自动使用深色模式并生成深色表面", () => {
  const tokens = buildTokens(
    customTheme({
      primary: "#6C8FB5",
      background: "#101418",
      surface: "#1B222A",
      text: "#E6EAF0",
      textMuted: "#8E9AA8",
      border: "#2B343E",
      success: "#7FBF9A",
      warning: "#D9B36C",
      danger: "#E09189",
    })
  );
  assert.equal(tokens.dark, true);
  assert.ok(contrast(tokens.ink, tokens.bgTop) >= 7);
  assert.equal(tokens.shadowRgb, "0, 0, 0");
  assert.ok(tokens.inputBg.startsWith("#"));
  assert.ok(relativeLuminance(tokens.bgBottom) < 0.1);
});

test("预设深色主题覆盖所有主要表面", () => {
  const midnight = BUILTIN_THEMES.find((theme) => theme.id === "midnight");
  const tokens = buildTokens(midnight);
  assert.equal(tokens.dark, true);
  assert.ok(contrast(tokens.ink, tokens.surfaceSolid) >= 7);
  assert.ok(contrast(tokens.inkFaint, tokens.bgTop) >= 4);
  const overrides = createThemeOverrides(tokens);
  assert.equal(overrides.common.textColorBase, "#E8ECF1");
  assert.equal(overrides.common.cardColor, "#262D36");
  assert.equal(overrides.common.inputColor, "#1D232B");
});

test("圆角被限制在 0–40 且保持递增", () => {
  assert.deepEqual(normalizeRadius({ small: 6, medium: 10, large: 14 }), {
    small: 6,
    medium: 10,
    large: 14,
  });
  assert.deepEqual(normalizeRadius({ small: 20, medium: 8, large: 99 }), {
    small: 20,
    medium: 20,
    large: 40,
  });
});

test("cssVariables 与 Naive 覆盖包含主题关键字段", () => {
  const tokens = buildTokens(BUILTIN_THEMES.find((theme) => theme.id === "snow"));
  const vars = cssVariables(tokens);
  for (const name of [
    "--haven-bg-top",
    "--haven-surface",
    "--haven-scrollbar",
    "--haven-radius-small",
  ]) {
    assert.ok(vars[name], `${name} 缺失`);
  }
  const overrides = createThemeOverrides(tokens);
  assert.equal(overrides.common.primaryColor, "#4A76A8");
  assert.equal(overrides.common.borderRadius, "10px");
  assert.equal(overrides.Input.textColor, tokens.ink);
  assert.equal(overrides.Button.textColorPrimary, "#FFFFFF");
});

test("mix 与对比色选择符合预期", () => {
  assert.equal(mix("#000000", "#FFFFFF", 0.5), "#808080");
  assert.equal(mix("#000000", "#FFFFFF", 0), "#000000");
  assert.equal(contrastText("#111111"), "#FFFFFF");
  assert.equal(contrastText("#F5F1E8"), "#171512");
});
