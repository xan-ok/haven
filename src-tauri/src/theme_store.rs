//! 主题文件存储与校验。
//!
//! 主题与保险库完全隔离：仅位于应用配置目录下的 `themes/` 与 `theme-settings.json`。
//! 导入只接受有限的 JSON 数据，拒绝未知字段、可执行内容与外部资源引用。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 单个主题文件大小上限：256 KB。
pub const MAX_THEME_BYTES: u64 = 256 * 1024;
const THEMES_DIR: &str = "themes";
const SETTINGS_FILE: &str = "theme-settings.json";
const THEME_FORMAT: &str = "haven-theme";
const THEME_VERSION: u32 = 1;

/// 内置主题标识，禁止被自定义主题覆盖。
const BUILTIN_IDS: [&str; 6] = [
    "haven-original",
    "snow",
    "midnight",
    "forest",
    "sand",
    "lavender",
];

#[derive(Debug, Serialize)]
pub struct ThemeError {
    pub code: &'static str,
    pub message: String,
}

impl ThemeError {
    fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }

    fn invalid(message: &str) -> Self {
        Self::new("THEME_INVALID", message)
    }

    pub(crate) fn disk() -> Self {
        Self::new("THEME_DISK", "无法读写主题文件，请检查磁盘空间与权限。")
    }

    fn exists() -> Self {
        Self::new(
            "THEME_EXISTS",
            "已存在相同标识的主题，请选择覆盖或修改主题 ID。",
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeColors {
    pub primary: String,
    pub background: String,
    pub surface: String,
    pub text: String,
    pub text_muted: String,
    pub border: String,
    pub success: String,
    pub warning: String,
    pub danger: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeRadius {
    pub small: u32,
    pub medium: u32,
    pub large: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeFile {
    pub format: String,
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub colors: ThemeColors,
    pub radius: ThemeRadius,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ThemeSettings {
    #[serde(default)]
    active_theme_id: Option<String>,
}

pub fn themes_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(THEMES_DIR)
}

fn theme_path(config_dir: &Path, id: &str) -> PathBuf {
    themes_dir(config_dir).join(format!("{id}.haventheme"))
}

/// 列出可读取的自定义主题；损坏的文件会被跳过，不影响应用启动。
pub fn list_user_themes(config_dir: &Path) -> Result<Vec<ThemeFile>, ThemeError> {
    let dir = themes_dir(config_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(ThemeError::disk()),
    };

    let mut themes = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("haventheme") {
            continue;
        }
        if let Ok(theme) = read_theme_file(&path) {
            themes.push(theme);
        }
    }
    themes.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(themes)
}

pub fn read_user_theme(config_dir: &Path, id: &str) -> Result<ThemeFile, ThemeError> {
    validate_id(id)?;
    read_theme_file(&theme_path(config_dir, id))
}

fn read_theme_file(path: &Path) -> Result<ThemeFile, ThemeError> {
    let metadata =
        fs::metadata(path).map_err(|_| ThemeError::invalid("主题文件不存在或无法读取。"))?;
    if metadata.len() > MAX_THEME_BYTES {
        return Err(ThemeError::invalid("主题文件超过 256 KB 限制。"));
    }
    let bytes = fs::read(path).map_err(|_| ThemeError::invalid("主题文件无法读取。"))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| ThemeError::invalid("主题文件必须是 UTF-8 编码的 JSON。"))?;
    let theme: ThemeFile = serde_json::from_str(text)
        .map_err(|_| ThemeError::invalid("主题文件结构不正确或包含不支持的字段。"))?;
    validate_theme(&theme)?;
    Ok(theme)
}

/// 从本地 `.haventheme` 文件导入主题，仅在全部校验通过后写入主题目录。
pub fn import_theme_file(
    config_dir: &Path,
    source: &Path,
    overwrite: bool,
) -> Result<ThemeFile, ThemeError> {
    if source.extension().and_then(|value| value.to_str()) != Some("haventheme") {
        return Err(ThemeError::invalid(
            "请选择扩展名为 .haventheme 的主题文件。",
        ));
    }
    let theme = read_theme_file(source)?;

    let destination = theme_path(config_dir, &theme.id);
    if destination.exists() && !overwrite {
        return Err(ThemeError::exists());
    }
    fs::create_dir_all(themes_dir(config_dir)).map_err(|_| ThemeError::disk())?;
    write_atomic(
        &destination,
        &serde_json::to_vec_pretty(&theme).map_err(|_| ThemeError::disk())?,
    )?;
    Ok(theme)
}

pub fn delete_user_theme(config_dir: &Path, id: &str) -> Result<(), ThemeError> {
    validate_id(id)?;
    let path = theme_path(config_dir, id);
    if !path.exists() {
        return Err(ThemeError::invalid("主题不存在或已被删除。"));
    }
    fs::remove_file(path).map_err(|_| ThemeError::disk())
}

/// 读取当前主题标识；文件缺失或损坏时回退为 None，不阻止应用启动。
pub fn get_active_theme(config_dir: &Path) -> Option<String> {
    let bytes = fs::read(config_dir.join(SETTINGS_FILE)).ok()?;
    let settings: ThemeSettings = serde_json::from_slice(&bytes).ok()?;
    settings.active_theme_id
}

pub fn set_active_theme(config_dir: &Path, id: Option<&str>) -> Result<(), ThemeError> {
    if let Some(value) = id {
        validate_id(value)?;
    }
    fs::create_dir_all(config_dir).map_err(|_| ThemeError::disk())?;
    let settings = ThemeSettings {
        active_theme_id: id.map(str::to_string),
    };
    let bytes = serde_json::to_vec_pretty(&settings).map_err(|_| ThemeError::disk())?;
    write_atomic(&config_dir.join(SETTINGS_FILE), &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ThemeError> {
    let parent = path.parent().ok_or_else(ThemeError::disk)?;
    fs::create_dir_all(parent).map_err(|_| ThemeError::disk())?;
    let temp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("theme")
    ));
    fs::write(&temp, bytes).map_err(|_| ThemeError::disk())?;
    fs::rename(&temp, path).map_err(|_| {
        let _ = fs::remove_file(&temp);
        ThemeError::disk()
    })
}

fn validate_theme(theme: &ThemeFile) -> Result<(), ThemeError> {
    if theme.format != THEME_FORMAT {
        return Err(ThemeError::invalid("主题格式标识必须为 haven-theme。"));
    }
    if theme.version != THEME_VERSION {
        return Err(ThemeError::invalid("不支持的主题版本，仅支持 version 1。"));
    }
    validate_id(&theme.id)?;
    if BUILTIN_IDS.contains(&theme.id.as_str()) {
        return Err(ThemeError::invalid("主题 ID 与内置主题冲突，请更换 ID。"));
    }
    validate_text("主题名称", &theme.name, 40, false)?;
    if let Some(description) = &theme.description {
        validate_text("主题描述", description, 200, true)?;
    }
    if let Some(author) = &theme.author {
        validate_text("作者", author, 40, true)?;
    }

    for (label, value) in [
        ("primary", &theme.colors.primary),
        ("background", &theme.colors.background),
        ("surface", &theme.colors.surface),
        ("text", &theme.colors.text),
        ("textMuted", &theme.colors.text_muted),
        ("border", &theme.colors.border),
        ("success", &theme.colors.success),
        ("warning", &theme.colors.warning),
        ("danger", &theme.colors.danger),
    ] {
        if !is_hex_color(value) {
            return Err(ThemeError::invalid(&format!(
                "颜色 {label} 必须是 #RRGGBB 格式。"
            )));
        }
    }

    for (label, value) in [
        ("small", theme.radius.small),
        ("medium", theme.radius.medium),
        ("large", theme.radius.large),
    ] {
        if value > 40 {
            return Err(ThemeError::invalid(&format!(
                "圆角 {label} 必须在 0 到 40 之间。"
            )));
        }
    }
    if !(theme.radius.small <= theme.radius.medium && theme.radius.medium <= theme.radius.large) {
        return Err(ThemeError::invalid("圆角需满足 small ≤ medium ≤ large。"));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), ThemeError> {
    let bytes = id.as_bytes();
    if bytes.len() < 2 || bytes.len() > 64 {
        return Err(ThemeError::invalid("主题 ID 长度需为 2 到 64 个字符。"));
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err(ThemeError::invalid(
            "主题 ID 只能包含小写字母、数字和连字符，且需以字母或数字开头。",
        ));
    }
    if !bytes
        .iter()
        .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || *value == b'-')
    {
        return Err(ThemeError::invalid(
            "主题 ID 只能包含小写字母、数字和连字符。",
        ));
    }
    Ok(())
}

fn validate_text(
    label: &str,
    value: &str,
    max_chars: usize,
    allow_empty: bool,
) -> Result<(), ThemeError> {
    let trimmed = value.trim();
    if trimmed.is_empty() && !allow_empty {
        return Err(ThemeError::invalid(&format!("{label}不能为空。")));
    }
    if trimmed.chars().count() > max_chars {
        return Err(ThemeError::invalid(&format!(
            "{label}不能超过 {max_chars} 个字符。"
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ThemeError::invalid(&format!("{label}包含不可见控制字符。")));
    }
    let lowered = trimmed.to_ascii_lowercase();
    for pattern in [
        "<",
        ">",
        "javascript:",
        "url(",
        "http://",
        "https://",
        "data:",
        "@import",
        "expression(",
        "{",
        "}",
        "\\",
    ] {
        if lowered.contains(pattern) {
            return Err(ThemeError::invalid(&format!(
                "{label}包含不允许的脚本或外部资源内容。"
            )));
        }
    }
    Ok(())
}

fn is_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_json() -> String {
        serde_json::json!({
            "format": "haven-theme",
            "version": 1,
            "id": "ocean-demo",
            "name": "Ocean",
            "description": "A calm ocean-inspired theme",
            "author": "Tester",
            "colors": {
                "primary": "#4A90A4",
                "background": "#F2F7F8",
                "surface": "#FFFFFF",
                "text": "#24343A",
                "textMuted": "#71858C",
                "border": "#DCE7EA",
                "success": "#438A70",
                "warning": "#C28B45",
                "danger": "#C65B5B"
            },
            "radius": { "small": 6, "medium": 10, "large": 14 }
        })
        .to_string()
    }

    fn write_theme(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).expect("write theme");
        path
    }

    #[test]
    fn imports_valid_theme() {
        let temp = tempfile::tempdir().expect("temp");
        let source = write_theme(temp.path(), "ocean.haventheme", &valid_json());
        let theme = import_theme_file(&temp.path().join("config"), &source, false).expect("import");
        assert_eq!(theme.name, "Ocean");
        assert!(
            temp.path()
                .join("config/themes/ocean-demo.haventheme")
                .exists()
        );
    }

    #[test]
    fn rejects_invalid_json_and_unknown_fields() {
        let temp = tempfile::tempdir().expect("temp");
        let bad = write_theme(temp.path(), "bad.haventheme", "{ broken");
        assert_eq!(
            import_theme_file(temp.path(), &bad, false)
                .expect_err("invalid")
                .code,
            "THEME_INVALID"
        );

        let mut value: serde_json::Value = serde_json::from_str(&valid_json()).expect("json");
        value["script"] = serde_json::json!("alert(1)");
        let extra = write_theme(temp.path(), "extra.haventheme", &value.to_string());
        assert!(import_theme_file(temp.path(), &extra, false).is_err());
    }

    #[test]
    fn rejects_missing_fields_and_bad_version() {
        let temp = tempfile::tempdir().expect("temp");
        let mut value: serde_json::Value = serde_json::from_str(&valid_json()).expect("json");
        value.as_object_mut().expect("object").remove("colors");
        let missing = write_theme(temp.path(), "missing.haventheme", &value.to_string());
        assert!(import_theme_file(temp.path(), &missing, false).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&valid_json()).expect("json");
        value["version"] = serde_json::json!(2);
        let version = write_theme(temp.path(), "v2.haventheme", &value.to_string());
        assert!(
            import_theme_file(temp.path(), &version, false)
                .expect_err("version")
                .message
                .contains("版本")
        );
    }

    #[test]
    fn rejects_bad_colors_and_radius() {
        let temp = tempfile::tempdir().expect("temp");
        let mut value: serde_json::Value = serde_json::from_str(&valid_json()).expect("json");
        value["colors"]["primary"] = serde_json::json!("blue");
        let color = write_theme(temp.path(), "color.haventheme", &value.to_string());
        assert!(import_theme_file(temp.path(), &color, false).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&valid_json()).expect("json");
        value["radius"]["large"] = serde_json::json!(99);
        let radius = write_theme(temp.path(), "radius.haventheme", &value.to_string());
        assert!(import_theme_file(temp.path(), &radius, false).is_err());
    }

    #[test]
    fn rejects_script_url_and_unsafe_id() {
        let temp = tempfile::tempdir().expect("temp");
        let mut value: serde_json::Value = serde_json::from_str(&valid_json()).expect("json");
        value["name"] = serde_json::json!("<script>alert(1)</script>");
        let script = write_theme(temp.path(), "script.haventheme", &value.to_string());
        assert!(import_theme_file(temp.path(), &script, false).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&valid_json()).expect("json");
        value["description"] = serde_json::json!("see https://example.com/theme.css");
        let url = write_theme(temp.path(), "url.haventheme", &value.to_string());
        assert!(import_theme_file(temp.path(), &url, false).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&valid_json()).expect("json");
        value["id"] = serde_json::json!("../escape");
        let id = write_theme(temp.path(), "id.haventheme", &value.to_string());
        assert!(import_theme_file(temp.path(), &id, false).is_err());
    }

    #[test]
    fn rejects_oversized_and_wrong_extension() {
        let temp = tempfile::tempdir().expect("temp");
        let mut body = valid_json();
        body.push_str(&" ".repeat((MAX_THEME_BYTES + 16) as usize));
        let big = write_theme(temp.path(), "big.haventheme", &body);
        assert!(
            import_theme_file(temp.path(), &big, false)
                .expect_err("size")
                .message
                .contains("256")
        );

        let wrong = write_theme(temp.path(), "plain.json", &valid_json());
        assert!(import_theme_file(temp.path(), &wrong, false).is_err());
    }

    #[test]
    fn rejects_builtin_id_and_conflicts() {
        let temp = tempfile::tempdir().expect("temp");
        let mut value: serde_json::Value = serde_json::from_str(&valid_json()).expect("json");
        value["id"] = serde_json::json!("midnight");
        let builtin = write_theme(temp.path(), "builtin.haventheme", &value.to_string());
        assert!(import_theme_file(temp.path(), &builtin, false).is_err());

        let source = write_theme(temp.path(), "ocean.haventheme", &valid_json());
        import_theme_file(temp.path(), &source, false).expect("first");
        assert_eq!(
            import_theme_file(temp.path(), &source, false)
                .expect_err("conflict")
                .code,
            "THEME_EXISTS"
        );
        import_theme_file(temp.path(), &source, true).expect("overwrite");
    }

    #[test]
    fn lists_and_deletes_user_themes() {
        let temp = tempfile::tempdir().expect("temp");
        let config = temp.path().join("config");
        let source = write_theme(temp.path(), "ocean.haventheme", &valid_json());
        import_theme_file(&config, &source, false).expect("import");
        assert_eq!(list_user_themes(&config).expect("list").len(), 1);
        delete_user_theme(&config, "ocean-demo").expect("delete");
        assert!(list_user_themes(&config).expect("list").is_empty());
        assert!(delete_user_theme(&config, "ocean-demo").is_err());
    }

    #[test]
    fn list_skips_broken_theme_files() {
        let temp = tempfile::tempdir().expect("temp");
        let config = temp.path().join("config");
        fs::create_dir_all(themes_dir(&config)).expect("mkdir");
        fs::write(themes_dir(&config).join("broken.haventheme"), "{").expect("write");
        assert!(list_user_themes(&config).expect("list").is_empty());
    }

    #[test]
    fn preference_roundtrip_and_corruption_fallback() {
        let temp = tempfile::tempdir().expect("temp");
        assert_eq!(get_active_theme(temp.path()), None);
        set_active_theme(temp.path(), Some("ocean-demo")).expect("set");
        assert_eq!(
            get_active_theme(temp.path()),
            Some("ocean-demo".to_string())
        );
        fs::write(temp.path().join(SETTINGS_FILE), "{ broken").expect("corrupt");
        assert_eq!(get_active_theme(temp.path()), None);
        assert!(set_active_theme(temp.path(), Some("../bad")).is_err());
    }
}
