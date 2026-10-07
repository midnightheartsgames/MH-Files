//! Категории сортировщика — содержимое `categories.json`, того же формата, что у MH Sort:
//! файл можно переносить между программами. Чтение и запись файла — в `mh-files-fs`.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "categories.json";

/// Переместить или скопировать.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Move,
    Copy,
}

/// Содержимое categories.json.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Категория для файлов, которые ни к чему не подошли.
    pub unknown_category: String,
    /// Папка типа для файлов без расширения.
    pub no_extension_folder: String,
    /// Такие файлы не трогаются: недокачанные и временные.
    pub ignore_extensions: Vec<String>,
    /// Категория → расширения. Если расширение указано дважды, побеждает первая категория.
    pub categories: IndexMap<String, Vec<String>>,
    /// Правила по дате и размеру — проверяются раньше категорий. MH Sort этого поля не знает
    /// и просто пропускает его.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
}

/// Правило: файл, подошедший по всем заданным условиям, идёт не в `<категория>`, а в
/// `<папка>\<категория>` — «старше года — в Архив», «больше 4 ГБ — в Большие».
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    /// Папка верхнего уровня рядом с категориями.
    pub folder: String,
    /// Не менялся дольше стольких дней.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub older_than_days: Option<u32>,
    /// Больше стольких мегабайт.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub larger_than_mb: Option<u64>,
    /// Только для этих категорий; пусто — для всех.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
}

impl Rule {
    /// Подходит ли файл: категория, размер в байтах, возраст в днях (`None` — неизвестен).
    /// Правило без условий не подходит ни к чему.
    pub fn matches(&self, category: &str, size: u64, age_days: Option<u64>) -> bool {
        if self.older_than_days.is_none() && self.larger_than_mb.is_none() {
            return false;
        }
        let category = category.to_lowercase();
        let category_ok = self.categories.is_empty()
            || self.categories.iter().any(|c| c.trim().to_lowercase() == category);
        let old_ok = self
            .older_than_days
            .is_none_or(|days| age_days.is_some_and(|age| age >= u64::from(days)));
        let large_ok = self.larger_than_mb.is_none_or(|mb| size > mb.saturating_mul(1 << 20));
        category_ok && old_ok && large_ok
    }
}

const DEFAULT_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "Изображения",
        &[
            "jpg", "jpeg", "jfif", "png", "webp", "gif", "bmp", "tif", "tiff", "svg", "ico",
            "heic", "heif", "avif", "raw", "cr2", "cr3", "nef", "arw", "dng", "tga", "exr", "hdr",
            "dds",
        ],
    ),
    (
        "Видео",
        &[
            "mp4", "mkv", "avi", "mov", "webm", "mpg", "mpeg", "m4v", "wmv", "flv", "3gp", "m2ts",
            "mts", "vob", "srt", "ass", "vtt",
        ],
    ),
    (
        "Аудио",
        &["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus", "wma", "aiff", "ape", "mid", "midi"],
    ),
    ("Документы", &["txt", "pdf", "doc", "docx", "odt", "rtf", "md", "log", "xps", "mht", "mhtml"]),
    ("Книги", &["epub", "fb2", "mobi", "azw3", "djvu", "cbz", "cbr"]),
    ("Таблицы", &["xls", "xlsx", "ods", "csv", "tsv"]),
    ("Презентации", &["ppt", "pptx", "odp"]),
    (
        "Архивы",
        &[
            "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "tar.gz", "tar.xz",
            "tar.bz2", "tar.zst",
        ],
    ),
    ("Образы дисков", &["iso", "img", "vhd", "vhdx", "vmdk", "dmg"]),
    (
        "3D",
        &[
            "blend", "blend1", "fbx", "obj", "mtl", "gltf", "glb", "dae", "stl", "3ds", "max",
            "ma", "mb", "usd", "usda", "usdc", "usdz", "abc", "ply", "3mf", "c4d", "ztl", "zpr",
            "vox",
        ],
    ),
    (
        "Проекты",
        &[
            "psd", "psb", "kra", "clip", "sai", "sai2", "xcf", "ai", "afphoto", "afdesign", "aep",
            "prproj", "drp", "spp", "sbs", "sbsar", "indd", "fla",
        ],
    ),
    (
        "Код",
        &[
            "py", "js", "ts", "jsx", "tsx", "c", "cpp", "h", "hpp", "rs", "java", "kt", "cs", "go",
            "rb", "php", "lua", "swift", "sh", "bat", "cmd", "ps1", "html", "css", "json", "xml",
            "yaml", "yml", "toml", "ini", "sql", "gd", "shader", "hlsl", "glsl",
        ],
    ),
    (
        "Игровые ресурсы",
        &["pak", "wad", "vpk", "unitypackage", "uasset", "umap", "bsa", "ba2", "esp", "esm", "rpf"],
    ),
    (
        "Программы",
        &[
            "exe",
            "msi",
            "appx",
            "msix",
            "appxbundle",
            "msixbundle",
            "apk",
            "xapk",
            "jar",
            "deb",
            "rpm",
            "appimage",
        ],
    ),
    ("AI модели", &["safetensors", "gguf", "ggml", "ckpt", "pt", "pth", "onnx", "tflite", "keras"]),
    ("Торренты", &["torrent"]),
    ("Шрифты", &["ttf", "otf", "woff", "woff2", "ttc", "fon"]),
    ("Ярлыки", &["lnk", "url"]),
];

impl Default for Config {
    fn default() -> Self {
        let words = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Self {
            unknown_category: "Прочее".into(),
            no_extension_folder: "Без расширения".into(),
            ignore_extensions: words(&[
                "crdownload",
                "part",
                "partial",
                "download",
                "opdownload",
                "downloading",
                "tmp",
                "!ut",
                "!qb",
                "aria2",
            ]),
            categories: DEFAULT_CATEGORIES
                .iter()
                .map(|(name, exts)| (name.to_string(), words(exts)))
                .collect(),
            rules: Vec::new(),
        }
    }
}

impl Config {
    /// Разбор categories.json. BOM в начале (Блокнот) не мешает.
    pub fn parse(text: &str) -> Result<Config, String> {
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())
    }

    /// JSON, удобный для ручной правки: каждая категория в одну строку.
    pub fn to_json(&self) -> String {
        let quote = |s: &str| serde_json::to_string(s).unwrap_or_default();
        let list = |items: &[String]| items.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", ");
        let mut out = String::from("{\n");
        out += &format!("  \"unknown_category\": {},\n", quote(&self.unknown_category));
        out += &format!("  \"no_extension_folder\": {},\n", quote(&self.no_extension_folder));
        out += &format!("  \"ignore_extensions\": [{}],\n", list(&self.ignore_extensions));
        if !self.rules.is_empty() {
            out += "  \"rules\": [\n";
            let last = self.rules.len() - 1;
            for (i, rule) in self.rules.iter().enumerate() {
                let comma = if i == last { "" } else { "," };
                out += &format!("    {}{comma}\n", serde_json::to_string(rule).unwrap_or_default());
            }
            out += "  ],\n";
        }
        out += "  \"categories\": {\n";
        let last = self.categories.len().saturating_sub(1);
        for (i, (name, exts)) in self.categories.iter().enumerate() {
            let comma = if i == last { "" } else { "," };
            out += &format!("    {}: [{}]{comma}\n", quote(name), list(exts));
        }
        out += "  }\n}\n";
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrip() {
        let config = Config::default();
        let parsed = Config::parse(&config.to_json()).unwrap();
        assert_eq!(parsed.categories, config.categories);
        assert_eq!(parsed.unknown_category, "Прочее");
        assert_eq!(parsed.categories.keys().next().unwrap(), "Изображения");
        assert!(Config::parse("{ broken").is_err());
        let with_bom = format!("\u{feff}{}", config.to_json());
        assert!(Config::parse(&with_bom).is_ok());
        assert!(!config.to_json().contains("rules"), "без правил — как у MH Sort");
    }

    #[test]
    fn rules_roundtrip_and_match() {
        let config = Config {
            rules: vec![
                Rule { folder: "Архив".into(), older_than_days: Some(365), ..Rule::default() },
                Rule {
                    folder: "Большие".into(),
                    larger_than_mb: Some(1024),
                    categories: vec!["Видео".into()],
                    ..Rule::default()
                },
            ],
            ..Config::default()
        };
        let parsed = Config::parse(&config.to_json()).unwrap();
        assert_eq!(parsed.rules, config.rules);
        let old = &parsed.rules[0];
        assert!(old.matches("Документы", 1, Some(400)));
        assert!(!old.matches("Документы", 1, Some(10)));
        assert!(!old.matches("Документы", 1, None), "возраст неизвестен — не подходит");
        let big = &parsed.rules[1];
        assert!(big.matches("видео", 2 << 30, None));
        assert!(!big.matches("Аудио", 2 << 30, None));
        assert!(!big.matches("Видео", 1 << 30, None), "ровно 1 ГБ — не больше");
        assert!(!Rule { folder: "X".into(), ..Rule::default() }.matches("Видео", 1, Some(1)));
    }
}
