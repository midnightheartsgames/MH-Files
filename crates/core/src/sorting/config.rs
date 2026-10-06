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
    }
}
