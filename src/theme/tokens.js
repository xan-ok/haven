// 设计变量：把主题色板转换为全局 CSS 变量与 Naive UI 主题覆盖。
// 内置主题使用预设 tokens；自定义主题由 9 个基础颜色推导，保证深色/浅色都可读。

const HEX_PATTERN = /^#[0-9a-f]{6}$/i;
const RANGE_MAX_RADIUS = 40;

export const THEME_FONT_FAMILY =
  '-apple-system, BlinkMacSystemFont, "PingFang SC", "Helvetica Neue", Arial, sans-serif';

export function isHexColor(value) {
  return typeof value === "string" && HEX_PATTERN.test(value);
}

export function hexToRgb(hex) {
  return {
    r: parseInt(hex.slice(1, 3), 16),
    g: parseInt(hex.slice(3, 5), 16),
    b: parseInt(hex.slice(5, 7), 16),
  };
}

export function rgbTriple(hex) {
  const { r, g, b } = hexToRgb(hex);
  return `${r}, ${g}, ${b}`;
}

function channelToHex(value) {
  return Math.round(Math.min(255, Math.max(0, value)))
    .toString(16)
    .padStart(2, "0");
}

export function rgbToHex({ r, g, b }) {
  return `#${channelToHex(r)}${channelToHex(g)}${channelToHex(b)}`;
}

/** weight 为向 target 混合的比例（0–1）。 */
export function mix(from, to, weight) {
  const a = hexToRgb(from);
  const b = hexToRgb(to);
  const ratio = Math.min(1, Math.max(0, weight));
  return rgbToHex({
    r: a.r + (b.r - a.r) * ratio,
    g: a.g + (b.g - a.g) * ratio,
    b: a.b + (b.b - a.b) * ratio,
  });
}

export function relativeLuminance(hex) {
  const { r, g, b } = hexToRgb(hex);
  const linear = [r, g, b].map((value) => {
    const channel = value / 255;
    return channel <= 0.03928
      ? channel / 12.92
      : ((channel + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

export function isDarkColor(hex) {
  return relativeLuminance(hex) < 0.3;
}

export function rgba(triple, alpha) {
  return `rgba(${triple}, ${alpha})`;
}

export function contrastText(hex) {
  const luminance = relativeLuminance(hex);
  const onWhite = 1.05 / (luminance + 0.05);
  const onDark = (luminance + 0.05) / 0.05;
  return onWhite >= onDark ? "#FFFFFF" : "#171512";
}

export function normalizeRadius(radius) {
  const clamp = (value, fallback) => {
    const number = Number(value);
    if (!Number.isFinite(number)) return fallback;
    return Math.min(RANGE_MAX_RADIUS, Math.max(0, Math.round(number)));
  };
  const small = clamp(radius?.small, 8);
  const medium = Math.max(small, clamp(radius?.medium, 12));
  const large = Math.max(medium, clamp(radius?.large, 16));
  return { small, medium, large };
}

function deriveTokens(theme) {
  const colors = theme.colors;
  const radius = normalizeRadius(theme.radius);
  const dark = isDarkColor(colors.background);

  const ink = colors.text;
  const inkFaint = colors.textMuted;
  const inkSoft = mix(colors.text, colors.textMuted, 0.35);
  const lineRgb = rgbTriple(mix(colors.border, colors.text, 0.3));
  const accent = colors.primary;
  const accentRgb = rgbTriple(accent);
  const surfaceRgb = rgbTriple(colors.surface);
  const surfaceSolid = isDarkColor(colors.surface)
    ? mix(colors.surface, "#FFFFFF", 0.04)
    : colors.surface;
  const dangerRgb = rgbTriple(colors.danger);
  const shadowRgb = dark
    ? "0, 0, 0"
    : rgbTriple(mix(colors.text, colors.background, 0.2));

  return {
    dark,
    radius,
    bgTop: mix(colors.background, "#FFFFFF", dark ? 0.06 : 0.45),
    bgBottom: mix(colors.background, "#000000", 0.06),
    ink,
    inkSoft,
    inkFaint,
    line: rgba(lineRgb, dark ? 0.3 : 0.26),
    lineRgb,
    accent,
    accentHover: mix(accent, "#FFFFFF", 0.12),
    accentPressed: mix(accent, dark ? "#000000" : "#000000", 0.14),
    accentDeep: mix(accent, dark ? "#FFFFFF" : "#000000", 0.2),
    accentRgb,
    accentSoft: rgba(accentRgb, dark ? 0.18 : 0.09),
    accentContrast: contrastText(accent),
    surface: rgba(surfaceRgb, dark ? 0.8 : 0.74),
    surfaceSolid,
    surfaceRgb,
    surfaceStrong: rgba(surfaceRgb, dark ? 0.92 : 0.9),
    surfaceInset: rgba(
      rgbTriple(dark ? mix(colors.surface, "#000000", 0.18) : mix(colors.surface, "#000000", 0.03)),
      0.85
    ),
    danger: colors.danger,
    dangerRgb,
    dangerSoft: rgba(dangerRgb, dark ? 0.16 : 0.08),
    success: colors.success,
    warning: colors.warning,
    successRgb: rgbTriple(colors.success),
    warningRgb: rgbTriple(colors.warning),
    shadow: dark
      ? "0 18px 50px -28px rgba(0, 0, 0, 0.62)"
      : `0 18px 50px -28px ${rgba(shadowRgb, 0.32)}`,
    shadowRgb,
    glowRgb: dark
      ? rgbTriple(mix(colors.background, "#FFFFFF", 0.1))
      : rgbTriple(mix(colors.background, "#FFFFFF", 0.62)),
    overlayRgb: dark ? "0, 0, 0" : "46, 42, 38",
    scrollbar: rgba(rgbTriple(inkFaint), 0.42),
    inputBg: dark
      ? mix(colors.background, "#FFFFFF", 0.08)
      : mix(colors.surface, "#FFFFFF", 0.4),
  };
}

/** 返回可直接用于渲染的完整 tokens。 */
export function buildTokens(theme) {
  if (!theme) return buildTokens({ colors: {}, radius: {} });
  const radius = normalizeRadius(theme.radius);
  if (theme.tokens) {
    const tokens = theme.tokens;
    return {
      ...tokens,
      dark: Boolean(theme.dark),
      radius,
      accentHover:
        tokens.accentHover ?? mix(tokens.accent, "#FFFFFF", 0.12),
      accentPressed:
        tokens.accentPressed ?? mix(tokens.accent, "#000000", 0.14),
      inputBg:
        tokens.inputBg ??
        (theme.dark ? mix(tokens.bgTop, "#FFFFFF", 0.06) : tokens.surfaceSolid),
    };
  }
  return deriveTokens({ ...theme, radius });
}

export function cssVariables(tokens) {
  return {
    "--haven-bg-top": tokens.bgTop,
    "--haven-bg-bottom": tokens.bgBottom,
    "--haven-ink": tokens.ink,
    "--haven-ink-soft": tokens.inkSoft,
    "--haven-ink-faint": tokens.inkFaint,
    "--haven-line": tokens.line,
    "--haven-line-rgb": tokens.lineRgb,
    "--haven-accent": tokens.accent,
    "--haven-accent-deep": tokens.accentDeep,
    "--haven-accent-rgb": tokens.accentRgb,
    "--haven-accent-soft": tokens.accentSoft,
    "--haven-accent-contrast": tokens.accentContrast,
    "--haven-danger": tokens.danger,
    "--haven-danger-rgb": tokens.dangerRgb,
    "--haven-danger-soft": tokens.dangerSoft,
    "--haven-success": tokens.success,
    "--haven-success-rgb": tokens.successRgb,
    "--haven-warning": tokens.warning,
    "--haven-warning-rgb": tokens.warningRgb,
    "--haven-surface": tokens.surface,
    "--haven-surface-solid": tokens.surfaceSolid,
    "--haven-surface-rgb": tokens.surfaceRgb,
    "--haven-surface-strong": tokens.surfaceStrong,
    "--haven-surface-inset": tokens.surfaceInset,
    "--haven-shadow": tokens.shadow,
    "--haven-shadow-rgb": tokens.shadowRgb,
    "--haven-glow-rgb": tokens.glowRgb,
    "--haven-overlay-rgb": tokens.overlayRgb,
    "--haven-scrollbar": tokens.scrollbar,
    "--haven-radius-small": `${tokens.radius.small}px`,
    "--haven-radius-medium": `${tokens.radius.medium}px`,
    "--haven-radius-large": `${tokens.radius.large}px`,
    "--haven-radius-card": `${tokens.radius.large + 8}px`,
    "--haven-radius-panel": `${tokens.radius.large + 6}px`,
  };
}

export function createThemeOverrides(tokens) {
  const accentSoft = rgba(tokens.accentRgb, 0.18);
  return {
    common: {
      primaryColor: tokens.accent,
      primaryColorHover: tokens.accentHover,
      primaryColorPressed: tokens.accentPressed,
      primaryColorSuppl: tokens.accentHover,
      successColor: tokens.success,
      warningColor: tokens.warning,
      errorColor: tokens.danger,
      infoColor: tokens.accentDeep,
      textColorBase: tokens.ink,
      textColor1: tokens.ink,
      textColor2: tokens.inkSoft,
      textColor3: tokens.inkFaint,
      placeholderColor: tokens.inkFaint,
      placeholderColorDisabled: tokens.inkFaint,
      borderColor: tokens.line,
      dividerColor: tokens.line,
      cardColor: tokens.surfaceSolid,
      modalColor: tokens.surfaceSolid,
      popoverColor: tokens.surfaceSolid,
      tableColor: tokens.surfaceSolid,
      inputColor: tokens.inputBg,
      inputColorDisabled: tokens.surfaceInset,
      actionColor: tokens.surfaceInset,
      tagColor: tokens.accentSoft,
      borderRadius: `${tokens.radius.medium}px`,
      borderRadiusSmall: `${tokens.radius.small}px`,
      scrollbarColor: tokens.scrollbar,
      scrollbarColorHover: rgba(tokens.accentRgb, 0.6),
      fontFamily: THEME_FONT_FAMILY,
    },
    Button: {
      textColorPrimary: tokens.accentContrast,
      textColorHoverPrimary: tokens.accentContrast,
      textColorPressedPrimary: tokens.accentContrast,
      textColorFocusPrimary: tokens.accentContrast,
      textColor: tokens.inkSoft,
      textColorHover: tokens.accentDeep,
      textColorPressed: tokens.accentDeep,
      textColorFocus: tokens.accentDeep,
      border: `1px solid ${tokens.line}`,
      borderHover: `1px solid ${rgba(tokens.accentRgb, 0.5)}`,
      borderPressed: `1px solid ${rgba(tokens.accentRgb, 0.6)}`,
      borderFocus: `1px solid ${rgba(tokens.accentRgb, 0.5)}`,
      colorHover: accentSoft,
      colorPressed: accentSoft,
      colorFocus: "transparent",
    },
    Input: {
      color: tokens.inputBg,
      colorFocus: tokens.inputBg,
      border: `1px solid ${tokens.line}`,
      borderHover: `1px solid ${rgba(tokens.accentRgb, 0.45)}`,
      borderFocus: `1px solid ${tokens.accent}`,
      boxShadowFocus: `0 0 0 2px ${rgba(tokens.accentRgb, 0.18)}`,
      textColor: tokens.ink,
      placeholderColor: tokens.inkFaint,
      caretColor: tokens.accent,
    },
    Checkbox: {
      colorCheckbox: tokens.accent,
      checkMarkColor: tokens.accentContrast,
    },
  };
}
