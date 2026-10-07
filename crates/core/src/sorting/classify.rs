//! Определение категории и папки типа для файла. Чтения файла здесь нет: сигнатуру (первые
//! байты) по просьбе классификатора узнаёт вызывающий.

use super::config::Config;
use super::names::{looks_like_ext, primary_ext, sanitize_name};
use std::collections::{HashMap, HashSet};

pub struct Classifier {
    /// Расширение (может быть составным: «tar.gz») → индекс категории.
    by_ext: HashMap<String, usize>,
    /// Имена папок категорий в порядке из конфига.
    categories: Vec<String>,
    unknown: String,
    no_ext_folder: String,
    ignore: HashSet<String>,
    /// Имена папок категорий в нижнем регистре — чтобы узнавать уже отсортированное.
    folder_names: HashSet<String>,
    /// Наибольшее число точек в расширениях из конфига.
    max_dots: usize,
    /// Замечания к конфигу для показа в окне.
    pub warnings: Vec<String>,
    pub extension_count: usize,
}

pub struct Class {
    pub category: String,
    pub type_folder: String,
    /// Расширение, по которому имя делится на основу и хвост при переименовании.
    pub ext: String,
    /// Категория найдена по содержимому файла, а не по расширению.
    pub by_content: bool,
}

fn normalize_ext(ext: &str) -> String {
    ext.trim().trim_start_matches('.').to_lowercase()
}

fn type_folder(ext: &str) -> String {
    sanitize_name(&ext.to_uppercase())
}

impl Classifier {
    pub fn new(config: &Config) -> Self {
        let mut by_ext = HashMap::new();
        let mut owner: HashMap<String, &str> = HashMap::new();
        let mut categories = Vec::new();
        let mut warnings = Vec::new();
        let mut max_dots = 0;

        for (name, exts) in &config.categories {
            let folder = sanitize_name(name);
            if folder != *name {
                warnings.push(format!(
                    "Категория «{name}» будет папкой «{folder}»: в имени были недопустимые символы."
                ));
            }
            let index = categories.len();
            categories.push(folder);
            for ext in exts.iter().map(|e| normalize_ext(e)).filter(|e| !e.is_empty()) {
                if let Some(first) = owner.get(&ext) {
                    if *first != name.as_str() {
                        warnings.push(format!(
                            ".{ext} есть в «{first}» и «{name}» — используется «{first}»."
                        ));
                    }
                    continue;
                }
                max_dots = max_dots.max(ext.matches('.').count());
                owner.insert(ext.clone(), name);
                by_ext.insert(ext, index);
            }
        }

        let unknown = sanitize_name(&config.unknown_category);
        let folder_names = categories.iter().chain([&unknown]).map(|n| n.to_lowercase()).collect();
        Self {
            extension_count: by_ext.len(),
            by_ext,
            categories,
            unknown,
            no_ext_folder: sanitize_name(&config.no_extension_folder),
            ignore: config.ignore_extensions.iter().map(|e| normalize_ext(e)).collect(),
            folder_names,
            max_dots,
            warnings,
        }
    }

    /// Имена папок категорий в порядке из categories.json.
    pub fn categories(&self) -> &[String] {
        &self.categories
    }

    pub fn category_count(&self) -> usize {
        self.categories.len()
    }

    pub fn unknown(&self) -> &str {
        &self.unknown
    }

    /// Папка с таким именем — это папка категории (регистр не важен).
    pub fn is_category_folder(&self, name: &str) -> bool {
        self.folder_names.contains(&name.to_lowercase())
    }

    /// Файл не нужно трогать (например, ещё качается).
    pub fn is_ignored(&self, file_name: &str) -> bool {
        let ext = primary_ext(file_name);
        !ext.is_empty() && self.ignore.contains(&ext)
    }

    /// Категория для файла с именем `name`. `sniff` — тип по содержимому (расширение вроде
    /// «png»); его спрашивают, только если по имени не опознать.
    pub fn classify(&self, name: &str, sniff: impl FnOnce() -> Option<&'static str>) -> Class {
        let name = name.to_lowercase();
        let candidates = ext_candidates(&name);

        // 1. По расширению; самое длинное совпадение важнее: «tar.gz» раньше «gz».
        for ext in &candidates {
            if ext.matches('.').count() > self.max_dots {
                continue;
            }
            if let Some(&index) = self.by_ext.get(*ext) {
                return Class {
                    category: self.categories[index].clone(),
                    type_folder: type_folder(ext),
                    ext: ext.to_string(),
                    by_content: false,
                };
            }
        }
        let ext = candidates.last().map(|e| e.to_string()).unwrap_or_default();

        // 2. По сигнатуре (первым байтам) — для файлов без расширения или с незнакомым.
        if let Some(kind) = sniff()
            && let Some(&index) = self.by_ext.get(kind)
        {
            let folder_ext: &str = if ext.is_empty() { kind } else { &ext };
            return Class {
                category: self.categories[index].clone(),
                type_folder: type_folder(folder_ext),
                ext,
                by_content: true,
            };
        }

        // 3. Не опознан.
        Class {
            category: self.unknown.clone(),
            type_folder: if ext.is_empty() {
                self.no_ext_folder.clone()
            } else {
                type_folder(&ext)
            },
            ext,
            by_content: false,
        }
    }
}

/// Возможные расширения от длинного к короткому: «a.tar.gz» → ["tar.gz", "gz"].
fn ext_candidates(name: &str) -> Vec<&str> {
    let body = name.trim_start_matches('.');
    let mut out: Vec<&str> = body.match_indices('.').map(|(i, _)| &body[i + 1..]).collect();
    if !out.last().is_some_and(|last| looks_like_ext(last)) {
        return Vec::new();
    }
    out.retain(|e| !e.starts_with('.') && !e.chars().any(char::is_whitespace));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn none() -> Option<&'static str> {
        None
    }

    fn classifier() -> Classifier {
        let mut categories = IndexMap::new();
        categories.insert("Изображения".to_string(), vec!["jpg".into(), "PNG".into()]);
        categories.insert("Архивы".to_string(), vec!["gz".into(), ".tar.gz".into(), "zip".into()]);
        categories.insert("Код".to_string(), vec!["ts".into(), "jpg".into()]);
        categories.insert("Игры / ресурсы".to_string(), vec!["pak".into()]);
        Classifier::new(&Config { categories, ..Config::default() })
    }

    #[test]
    fn by_extension() {
        let c = classifier();
        let class = c.classify("Photo.JPG", none);
        assert_eq!((class.category.as_str(), class.type_folder.as_str()), ("Изображения", "JPG"));

        let class = c.classify("backup.2024.tar.gz", none);
        assert_eq!((class.category.as_str(), class.type_folder.as_str()), ("Архивы", "TAR.GZ"));
        assert_eq!(class.ext, "tar.gz");

        let class = c.classify("data.gz", none);
        assert_eq!(class.type_folder, "GZ");

        let class = c.classify("assets.pak", none);
        assert_eq!(class.category, "Игры _ ресурсы");
    }

    #[test]
    fn unknown_files() {
        let c = classifier();
        let class = c.classify("notes.xyz", none);
        assert_eq!((class.category.as_str(), class.type_folder.as_str()), ("Прочее", "XYZ"));

        let class = c.classify("Mr. Smith goes", none);
        assert_eq!(class.type_folder, "Без расширения");
        assert_eq!(class.ext, "");

        let class = c.classify("device.con", none);
        assert_eq!(class.type_folder, "CON_");
    }

    #[test]
    fn duplicates_and_names_are_reported() {
        let c = classifier();
        assert_eq!(c.warnings.len(), 2, "{:?}", c.warnings);
        assert!(c.is_category_folder("изображения"));
        assert!(c.is_category_folder("ПРОЧЕЕ"));
        assert!(!c.is_category_folder("Фото"));
    }

    #[test]
    fn ignored_downloads() {
        let c = classifier();
        assert!(c.is_ignored("movie.mp4.crdownload"));
        assert!(c.is_ignored("x.PART"));
        assert!(!c.is_ignored("part"));
    }

    #[test]
    fn detects_by_signature() {
        let c = classifier();
        let class = c.classify("download", || Some("png"));
        assert_eq!((class.category.as_str(), class.type_folder.as_str()), ("Изображения", "PNG"));
        assert!(class.by_content);

        let class = c.classify("picture.dat", || Some("png"));
        assert_eq!((class.category.as_str(), class.type_folder.as_str()), ("Изображения", "DAT"));

        let class = c.classify("download", none);
        assert_eq!(class.category, "Прочее");
        // Опознанное по имени содержимого не спрашивает.
        let class = c.classify("photo.jpg", || panic!("не нужно"));
        assert_eq!(class.category, "Изображения");
    }
}
