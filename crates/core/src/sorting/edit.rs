//! Правка категорий в настройках: черновик, разбор списков расширений, проверка.
//!
//! Расширения в черновике — текст, как его набрал пользователь («jpg, jpeg, *.PNG»): в
//! [`Config`] он превращается только при проверке и сохранении. Так поле ввода не
//! «перепрыгивает» под пальцами.

use indexmap::IndexMap;

use super::classify::Classifier;
use super::config::{Config, Rule};
use super::names::sanitize_name;

/// Категория в черновике.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftCategory {
    pub name: String,
    /// Расширения через запятую или пробел.
    pub extensions: String,
}

/// Правило по дате и размеру в черновике: числа — текстом, как их набрали.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftRule {
    pub folder: String,
    /// Дней; пусто — без условия.
    pub older_than_days: String,
    /// Мегабайт; пусто — без условия.
    pub larger_than_mb: String,
    /// Категории через запятую; пусто — все.
    pub categories: String,
}

/// Черновик categories.json.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub categories: Vec<DraftCategory>,
    pub unknown_category: String,
    pub no_extension_folder: String,
    /// Не трогать файлы с такими расширениями (недокачанные).
    pub ignore_extensions: String,
    pub rules: Vec<DraftRule>,
}

/// Число из поля: пусто — `Ok(None)`, не число — `Err`.
fn number<T: std::str::FromStr>(text: &str) -> Result<Option<T>, ()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse().map(Some).map_err(|_| ())
}

fn split_names(text: &str) -> Vec<String> {
    text.split([',', ';']).map(str::trim).filter(|n| !n.is_empty()).map(String::from).collect()
}

/// Итог проверки: ошибки не дают сохранить, предупреждения — нет.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Check {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Check {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Разобрать список расширений: разделители — запятая, точка с запятой и пробелы;
/// `*.mp4`, `.mp4` и `MP4` — одно и то же. Повторы убираются, порядок сохраняется.
pub fn parse_extensions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        let ext = word.trim().trim_start_matches('*').trim_start_matches('.').to_lowercase();
        if !ext.is_empty() && !out.contains(&ext) {
            out.push(ext);
        }
    }
    out
}

pub fn format_extensions(list: &[String]) -> String {
    list.join(", ")
}

/// Символы, которых не бывает в расширении Windows.
fn bad_extension(ext: &str) -> bool {
    ext.chars().any(|c| matches!(c, '\\' | '/' | ':' | '?' | '"' | '<' | '>' | '|' | '*'))
        || ext.ends_with('.')
}

impl Draft {
    pub fn from_config(config: &Config) -> Draft {
        Draft {
            categories: config
                .categories
                .iter()
                .map(|(name, exts)| DraftCategory {
                    name: name.clone(),
                    extensions: format_extensions(exts),
                })
                .collect(),
            unknown_category: config.unknown_category.clone(),
            no_extension_folder: config.no_extension_folder.clone(),
            ignore_extensions: format_extensions(&config.ignore_extensions),
            rules: config
                .rules
                .iter()
                .map(|rule| DraftRule {
                    folder: rule.folder.clone(),
                    older_than_days: rule
                        .older_than_days
                        .map(|d| d.to_string())
                        .unwrap_or_default(),
                    larger_than_mb: rule.larger_than_mb.map(|m| m.to_string()).unwrap_or_default(),
                    categories: rule.categories.join(", "),
                })
                .collect(),
        }
    }

    /// Конфиг из черновика. Имена — без пробелов по краям; категория с повторным именем
    /// сливается с первой (проверка об этом говорит ошибкой, сюда доходит только при ней).
    pub fn to_config(&self) -> Config {
        let mut categories: IndexMap<String, Vec<String>> = IndexMap::new();
        for category in &self.categories {
            let list = categories.entry(category.name.trim().to_string()).or_default();
            for ext in parse_extensions(&category.extensions) {
                if !list.contains(&ext) {
                    list.push(ext);
                }
            }
        }
        Config {
            unknown_category: self.unknown_category.trim().to_string(),
            no_extension_folder: self.no_extension_folder.trim().to_string(),
            ignore_extensions: parse_extensions(&self.ignore_extensions),
            categories,
            rules: self
                .rules
                .iter()
                .map(|rule| Rule {
                    folder: rule.folder.trim().to_string(),
                    older_than_days: number(&rule.older_than_days).ok().flatten(),
                    larger_than_mb: number(&rule.larger_than_mb).ok().flatten(),
                    categories: split_names(&rule.categories),
                })
                .collect(),
        }
    }

    /// Отличается ли черновик от конфига по смыслу (а не по написанию: «JPG, .png» и
    /// «jpg, png» — одно и то же).
    pub fn differs_from(&self, config: &Config) -> bool {
        self.to_config().to_json() != Draft::from_config(config).to_config().to_json()
    }

    pub fn check(&self) -> Check {
        let mut check = Check::default();
        let mut folders: Vec<(String, String)> = Vec::new();
        for (index, category) in self.categories.iter().enumerate() {
            let name = category.name.trim();
            if name.is_empty() {
                check.errors.push(format!("У категории №{} нет имени.", index + 1));
                continue;
            }
            let folder = sanitize_name(name).to_lowercase();
            if let Some((first, _)) = folders.iter().find(|(_, f)| *f == folder) {
                check.errors.push(format!("«{first}» и «{name}» — одна и та же папка."));
            } else {
                folders.push((name.to_string(), folder));
            }
            let exts = parse_extensions(&category.extensions);
            if exts.is_empty() {
                check.warnings.push(format!(
                    "В «{name}» нет расширений — туда попадёт только опознанное по содержимому."
                ));
            }
            for ext in exts.iter().filter(|e| bad_extension(e)) {
                check.errors.push(format!("«{ext}» в «{name}» — недопустимое расширение."));
            }
        }
        let unknown = self.unknown_category.trim();
        if unknown.is_empty() {
            check.errors.push("Нет имени папки для неопознанных файлов.".into());
        } else if let Some((name, _)) =
            folders.iter().find(|(_, f)| *f == sanitize_name(unknown).to_lowercase())
        {
            check
                .errors
                .push(format!("Папка неопознанных «{unknown}» совпадает с категорией «{name}»."));
        }
        if self.no_extension_folder.trim().is_empty() {
            check.errors.push("Нет имени папки для файлов без расширения.".into());
        }
        for ext in parse_extensions(&self.ignore_extensions).iter().filter(|e| bad_extension(e)) {
            check.errors.push(format!("«{ext}» в пропускаемых — недопустимое расширение."));
        }
        for (index, rule) in self.rules.iter().enumerate() {
            let folder = rule.folder.trim();
            let label = if folder.is_empty() {
                format!("№{}", index + 1)
            } else {
                format!("«{folder}»")
            };
            if folder.is_empty() {
                check.errors.push(format!("У правила {label} нет папки."));
            } else if let Some((name, _)) =
                folders.iter().find(|(_, f)| *f == sanitize_name(folder).to_lowercase())
            {
                check
                    .errors
                    .push(format!("Папка правила {label} совпадает с категорией «{name}»."));
            }
            let days = number::<u32>(&rule.older_than_days);
            let mb = number::<u64>(&rule.larger_than_mb);
            if days.is_err() {
                check.errors.push(format!("В правиле {label} дни — не число."));
            }
            if mb.is_err() {
                check.errors.push(format!("В правиле {label} мегабайты — не число."));
            }
            if matches!((days, mb), (Ok(None), Ok(None))) {
                check
                    .errors
                    .push(format!("У правила {label} нет условий: укажите дни или размер."));
            }
            for name in split_names(&rule.categories) {
                if !folders.iter().any(|(n, _)| n.to_lowercase() == name.to_lowercase()) {
                    check
                        .warnings
                        .push(format!("В правиле {label} нет такой категории: «{name}»."));
                }
            }
        }
        // Остальное — те же замечания, что увидит сортировщик: повторы расширений и
        // символы, недопустимые в именах папок.
        if check.is_ok() {
            check.warnings.extend(Classifier::new(&self.to_config()).warnings);
        }
        check
    }

    /// Новая категория в конце; возвращает её номер.
    pub fn add(&mut self) -> usize {
        let taken = |name: &str| {
            self.categories.iter().any(|c| c.name.trim().to_lowercase() == name.to_lowercase())
        };
        let mut name = "Новая категория".to_string();
        let mut n = 2;
        while taken(&name) {
            name = format!("Новая категория {n}");
            n += 1;
        }
        self.categories.push(DraftCategory { name, extensions: String::new() });
        self.categories.len() - 1
    }

    /// Поменять местами с соседом: порядок важен — при повторе расширения побеждает
    /// верхняя категория. Возвращает новый номер.
    pub fn move_by(&mut self, index: usize, up: bool) -> usize {
        let other = if up { index.checked_sub(1) } else { Some(index + 1) };
        match other.filter(|&o| o < self.categories.len() && index < self.categories.len()) {
            Some(other) => {
                self.categories.swap(index, other);
                other
            }
            None => index,
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.categories.len() {
            self.categories.remove(index);
        }
    }

    /// Новое правило «старше года — в Архив».
    pub fn add_rule(&mut self) {
        self.rules.push(DraftRule {
            folder: "Архив".into(),
            older_than_days: "365".into(),
            ..DraftRule::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_parsed_leniently() {
        assert_eq!(parse_extensions("*.MP4, .mkv;avi  webm,, mp4"), ["mp4", "mkv", "avi", "webm"]);
        assert_eq!(parse_extensions("tar.gz\n.TAR.XZ"), ["tar.gz", "tar.xz"]);
        assert!(parse_extensions(" , ; ").is_empty());
    }

    #[test]
    fn default_config_roundtrips_without_changes() {
        let config = Config::default();
        let draft = Draft::from_config(&config);
        assert!(!draft.differs_from(&config));
        assert_eq!(draft.to_config().categories, config.categories);
        assert!(draft.check().is_ok(), "{:?}", draft.check());
        assert!(draft.check().warnings.is_empty(), "{:?}", draft.check());
    }

    #[test]
    fn spelling_is_not_a_change() {
        let config = Config::default();
        let mut draft = Draft::from_config(&config);
        draft.categories[0].extensions = draft.categories[0].extensions.to_uppercase();
        draft.categories[0].name = format!("  {} ", draft.categories[0].name);
        assert!(!draft.differs_from(&config));
        draft.categories[0].extensions.push_str(", jxl");
        assert!(draft.differs_from(&config));
        assert!(draft.to_config().categories["Изображения"].contains(&"jxl".to_string()));
    }

    #[test]
    fn errors_block_and_warnings_do_not() {
        let mut draft = Draft::from_config(&Config::default());
        draft.categories[1].name = "изображения".into(); // та же папка, что у первой
        draft.categories[2].name = "  ".into();
        draft.categories[3].extensions = "doc, a/b".into();
        draft.unknown_category = "Видео".into(); // Видео переименовано — совпадения нет
        let check = draft.check();
        assert_eq!(check.errors.len(), 3, "{:?}", check.errors);

        let mut draft = Draft::from_config(&Config::default());
        draft.unknown_category = "ВИДЕО".into();
        assert_eq!(draft.check().errors.len(), 1);

        let mut draft = Draft::from_config(&Config::default());
        draft.categories[5].extensions.push_str(", jpg"); // jpg уже в «Изображениях»
        draft.categories[6].extensions.clear();
        let check = draft.check();
        assert!(check.is_ok());
        assert_eq!(check.warnings.len(), 2, "{:?}", check.warnings);
    }

    #[test]
    fn add_move_remove() {
        let mut draft = Draft::from_config(&Config::default());
        let count = draft.categories.len();
        let a = draft.add();
        let b = draft.add();
        assert_eq!((a, b), (count, count + 1));
        assert_eq!(draft.categories[b].name, "Новая категория 2");
        assert_eq!(draft.move_by(0, true), 0, "выше первой некуда");
        assert_eq!(draft.move_by(b, false), b, "ниже последней некуда");
        assert_eq!(draft.move_by(1, true), 0);
        assert_eq!(draft.categories[0].name, "Видео");
        draft.remove(a);
        assert_eq!(draft.categories.len(), count + 1);
        draft.remove(999);
        // Порядок решает, кому достаётся повторное расширение.
        draft.categories[0].extensions.push_str(", png");
        let config = draft.to_config();
        let class = Classifier::new(&config).classify("x.png", || None);
        assert_eq!(class.category, "Видео");
    }

    #[test]
    fn rules_in_draft() {
        let mut draft = Draft::from_config(&Config::default());
        draft.add_rule();
        assert!(draft.check().is_ok());
        let config = draft.to_config();
        assert_eq!(config.rules[0].older_than_days, Some(365));
        assert_eq!(Draft::from_config(&config).rules, draft.rules);
        draft.rules[0].older_than_days = "год".into();
        assert!(!draft.check().is_ok(), "не число");
        draft.rules[0].older_than_days.clear();
        assert!(!draft.check().is_ok(), "нет условий");
        draft.rules[0].larger_than_mb = "100".into();
        draft.rules[0].folder = "видео".into();
        assert!(!draft.check().is_ok(), "папка правила совпадает с категорией");
        draft.rules[0].folder = "Большие".into();
        draft.rules[0].categories = "Видео, Нет такой".into();
        let check = draft.check();
        assert!(check.is_ok());
        assert!(check.warnings.iter().any(|w| w.contains("Нет такой")));
    }
}
