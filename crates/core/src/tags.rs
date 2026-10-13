//! Автотеги медиафайлов (1.3): что изображено на картинке или кадре видео — словами из
//! словаря Danbooru («1girl», «cat», «outdoors»).
//!
//! Модель — WD Tagger v3 (SmilingWolf, Apache 2.0) в ONNX: на входе картинка 448×448, на
//! выходе уверенность для каждого из ~10 800 тегов словаря `selected_tags.csv`. Теги трёх
//! видов: обычные (что на картинке), персонажи и рейтинг (один из четырёх, по наибольшей
//! уверенности). Здесь — словарь, выбор тегов по порогам, вход модели и снимок тегов на
//! диске; без ввода-вывода.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::index::snapshot::{Reader, fnv};

/// Готовые модели: репозиторий Hugging Face, название, размер. У всех — `model.onnx` и
/// `selected_tags.csv` одного формата.
pub const MODELS: [ModelInfo; 3] = [
    ModelInfo {
        repo: "SmilingWolf/wd-eva02-large-tagger-v3",
        title: "EVA02-Large — точнее всего",
        size: "1,3 ГБ",
    },
    ModelInfo {
        repo: "SmilingWolf/wd-swinv2-tagger-v3",
        title: "SwinV2 — втрое быстрее",
        size: "470 МБ",
    },
    ModelInfo {
        repo: "SmilingWolf/wd-vit-tagger-v3",
        title: "ViT — быстрее всего",
        size: "380 МБ",
    },
];

pub const DEFAULT_MODEL: &str = MODELS[0].repo;
pub const MODEL_FILE: &str = "model.onnx";
pub const TAGS_FILE: &str = "selected_tags.csv";

/// ONNX Runtime со встроенным DirectML (видеокарта; на процессоре считает тоже) — пакеты
/// NuGet. DirectML у Microsoft на поддержке, его последняя сборка ONNX Runtime — 1.24.4.
pub const RUNTIME_VERSION: &str = "1.24.4";
/// Папка скачанных библиотек: другая сборка — другая папка, прежняя удаляется.
pub const RUNTIME_FOLDER: &str = "1.24.4-directml";
pub const NUGET_HOST: &str = "api.nuget.org";

/// Библиотеки ONNX Runtime в папке сборки; первая — та, что загружается.
pub const RUNTIME_FILES: [&str; 3] =
    ["onnxruntime.dll", "onnxruntime_providers_shared.dll", "DirectML.dll"];

/// Откуда берутся [`RUNTIME_FILES`]: пакет NuGet (путь на [`NUGET_HOST`]) и файлы в нём. Пакет
/// DirectML — 200 МБ сборок под все платформы и Xbox, а нужна одна DLL: файлы достаются из
/// пакетов кусками (HTTP Range), не скачивая пакет целиком.
pub fn runtime_packages() -> Vec<(String, &'static [(&'static str, &'static str)])> {
    let package =
        |id: &str, version: &str| format!("/v3-flatcontainer/{id}/{version}/{id}.{version}.nupkg");
    vec![
        (
            package("microsoft.ml.onnxruntime.directml", RUNTIME_VERSION),
            &[
                ("runtimes/win-x64/native/onnxruntime.dll", RUNTIME_FILES[0]),
                ("runtimes/win-x64/native/onnxruntime_providers_shared.dll", RUNTIME_FILES[1]),
            ],
        ),
        // Тот DirectML, с которым собран ONNX Runtime (зависимость его пакета).
        (
            package("microsoft.ai.directml", "1.15.4"),
            &[("bin/x64-win/DirectML.dll", RUNTIME_FILES[2])],
        ),
    ]
}

/// Где взять библиотеки Visual C++, без которых `onnxruntime.dll` не загружается.
pub const VC_REDIST: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";

/// Понятная причина, если ONNX Runtime не загрузился. Windows отвечает «module could not be
/// found» (ошибка 126) и тогда, когда сам файл на месте, но нет библиотек Visual C++
/// (`MSVCP140.dll`), — а файл проверяется до запуска.
pub fn explain_runtime_error(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("os error 126")
        || lower.contains("msvcp140")
        || lower.contains("vcruntime140")
        || lower.contains("could not be found")
    {
        format!(
            "ONNX Runtime не загрузился: нет библиотек Microsoft Visual C++ (x64). Поставьте их: {VC_REDIST} ({error})"
        )
    } else {
        format!("ONNX Runtime не загрузился: {error}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInfo {
    pub repo: &'static str,
    pub title: &'static str,
    pub size: &'static str,
}

/// Готовая модель по репозиторию.
pub fn model_info(repo: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.repo.eq_ignore_ascii_case(repo))
}

/// Имя папки модели: `SmilingWolf--wd-eva02-large-tagger-v3` — без `/` и прочего, что
/// нельзя в имени Windows.
pub fn model_folder(repo: &str) -> String {
    let part = |text: &str| -> String {
        text.chars()
            .map(
                |c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' },
            )
            .collect()
    };
    repo.split('/').map(part).collect::<Vec<_>>().join("--")
}

/// Сторона картинки на входе модели, если модель её не называет.
pub const INPUT_SIDE: usize = 448;

/// Ниже этой уверенности теги не хранятся: порог в настройках можно опустить до неё без
/// пересчёта.
pub const FLOOR: f32 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagKind {
    /// Что на картинке: предметы, место, вид, одежда.
    General,
    /// Персонаж (аниме, игры).
    Character,
    /// Рейтинг: general, sensitive, questionable, explicit.
    Rating,
}

impl TagKind {
    /// Категория из `selected_tags.csv`: 0 — обычный, 4 — персонаж, 9 — рейтинг.
    fn from_category(category: u32) -> Option<TagKind> {
        match category {
            0 => Some(TagKind::General),
            4 => Some(TagKind::Character),
            9 => Some(TagKind::Rating),
            _ => None,
        }
    }
}

/// Теги, где `_` — часть смайлика, а не пробел (как у SmilingWolf/wd-tagger).
const KAOMOJIS: [&str; 19] = [
    "0_0", "(o)_(o)", "+_+", "+_-", "._.", "<o>_<o>", "<|>_<|>", "=_=", ">_<", "3_3", "6_9", ">_o",
    "@_@", "^_^", "o_o", "u_u", "x_x", "|_|", "||_||",
];

/// Имя тега для показа: `long_hair` → `long hair`, смайлики как есть.
pub fn display_name(raw: &str) -> String {
    if KAOMOJIS.contains(&raw) { raw.to_string() } else { raw.replace('_', " ") }
}

/// Рейтинг по-русски.
pub fn rating_label(name: &str) -> &'static str {
    match name {
        "general" => "обычный",
        "sensitive" => "чувствительный",
        "questionable" => "сомнительный",
        "explicit" => "откровенный (18+)",
        _ => "неизвестный",
    }
}

/// Словарь модели: имена тегов (для показа) и их виды, по номеру выхода модели.
#[derive(Debug, Clone, PartialEq)]
pub struct Vocabulary {
    names: Vec<String>,
    kinds: Vec<TagKind>,
    /// Отпечаток списка: теги в снимке — номера, другой словарь их не поймёт.
    fingerprint: u64,
}

impl Vocabulary {
    /// Разобрать `selected_tags.csv`: `tag_id,name,category,count`, первая строка —
    /// заголовок. В именах запятых нет, кавычек тоже — разбор по запятым.
    pub fn parse(csv: &str) -> Result<Vocabulary, String> {
        let mut lines = csv.lines();
        let header = lines.next().unwrap_or_default();
        let columns: Vec<&str> = header.trim().split(',').collect();
        let find = |name: &str| columns.iter().position(|c| c.trim() == name);
        let (Some(name_at), Some(category_at)) = (find("name"), find("category")) else {
            return Err("в списке тегов нет столбцов name и category".into());
        };
        let mut names = Vec::new();
        let mut kinds = Vec::new();
        let mut raw = String::new();
        for (number, line) in lines.enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.trim_end_matches('\r').split(',').collect();
            let bad = || format!("строка {} списка тегов не читается", number + 2);
            let name = fields.get(name_at).ok_or_else(bad)?;
            let category: u32 =
                fields.get(category_at).ok_or_else(bad)?.parse().map_err(|_| bad())?;
            // Неизвестный вид — как обычный тег: номер выхода важнее.
            kinds.push(TagKind::from_category(category).unwrap_or(TagKind::General));
            names.push(display_name(name));
            raw.push_str(name);
            raw.push('\n');
        }
        if names.is_empty() {
            return Err("список тегов пуст".into());
        }
        Ok(Vocabulary { names, kinds, fingerprint: fnv(raw.as_bytes()) })
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub fn name(&self, index: usize) -> &str {
        self.names.get(index).map_or("", String::as_str)
    }

    pub fn kind(&self, index: usize) -> TagKind {
        self.kinds.get(index).copied().unwrap_or(TagKind::General)
    }
}

/// Уверенность 0..1 байтом: точности в полпроцента для порогов хватает.
fn quantize(score: f32) -> u8 {
    (score.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn dequantize(score: u8) -> f32 {
    f32::from(score) / 255.0
}

/// Теги файла: номер в словаре и уверенность байтом. Хранятся теги не ниже [`FLOOR`] и
/// один рейтинг — самый уверенный.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTags {
    pub scores: Vec<(u32, u8)>,
}

impl FileTags {
    /// Из выхода модели: уверенность каждого тега словаря.
    pub fn from_scores(scores: &[f32], vocabulary: &Vocabulary) -> Result<FileTags, String> {
        if scores.len() != vocabulary.len() {
            return Err(format!(
                "модель дала {} оценок, а в списке тегов {}",
                scores.len(),
                vocabulary.len()
            ));
        }
        let mut kept = Vec::new();
        let mut rating: Option<(usize, f32)> = None;
        for (index, &score) in scores.iter().enumerate() {
            let score = if score.is_finite() { score } else { 0.0 };
            match vocabulary.kind(index) {
                TagKind::Rating => {
                    if rating.is_none_or(|(_, best)| score > best) {
                        rating = Some((index, score));
                    }
                }
                _ if score >= FLOOR => kept.push((index as u32, quantize(score))),
                _ => {}
            }
        }
        if let Some((index, score)) = rating {
            kept.push((index as u32, quantize(score)));
        }
        // Самые уверенные первыми: так строка столбца собирается без сортировки.
        kept.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(FileTags { scores: kept })
    }
}

/// Пороги показа: тег виден, если уверенность не ниже порога своего вида.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub general: f32,
    pub character: f32,
}

impl Default for Thresholds {
    /// Как у SmilingWolf/wd-tagger: 0,35 и 0,85.
    fn default() -> Thresholds {
        Thresholds { general: 0.35, character: 0.85 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub name: String,
    pub kind: TagKind,
    pub score: f32,
}

/// Что показать у файла.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shown {
    /// Персонажи, затем обычные теги; внутри — по уверенности.
    pub tags: Vec<Tag>,
    pub rating: Option<Tag>,
    /// Строка для столбца «Теги»: имена через запятую.
    pub line: String,
}

/// Теги файла по порогам.
pub fn show(file: &FileTags, vocabulary: &Vocabulary, thresholds: Thresholds) -> Shown {
    let mut characters = Vec::new();
    let mut general = Vec::new();
    let mut rating = None;
    for &(index, score) in &file.scores {
        let index = index as usize;
        if index >= vocabulary.len() {
            continue;
        }
        let kind = vocabulary.kind(index);
        let tag = Tag { name: vocabulary.name(index).to_string(), kind, score: dequantize(score) };
        match kind {
            TagKind::Rating => rating = Some(tag),
            TagKind::Character if tag.score >= thresholds.character => characters.push(tag),
            TagKind::General if tag.score >= thresholds.general => general.push(tag),
            _ => {}
        }
    }
    let by_score = |a: &Tag, b: &Tag| b.score.total_cmp(&a.score).then(a.name.cmp(&b.name));
    characters.sort_by(by_score);
    general.sort_by(by_score);
    characters.extend(general);
    let line = characters.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ");
    Shown { tags: characters, rating, line }
}

/// Вход модели: картинка `side`×`side` в RGB (строки сверху вниз) → числа в порядке
/// NHWC, каналы BGR, значения 0–255 — так учился WD Tagger v3.
pub fn input_tensor(rgb: &[u8], side: usize) -> Option<Vec<f32>> {
    if rgb.len() != side * side * 3 {
        return None;
    }
    Some(rgb.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [b, g, r].map(f32::from)).collect())
}

/// Сторона картинки по форме входа модели `[N, H, W, 3]` (N и неизвестное — `-1`).
/// Другая форма — не WD Tagger: ошибка.
pub fn input_side(shape: &[i64]) -> Result<usize, String> {
    match shape {
        [_, h, w, 3] if h == w && *h > 0 => Ok(*h as usize),
        [_, -1, -1, 3] => Ok(INPUT_SIDE),
        _ => Err(format!(
            "у модели вход {shape:?}, а нужен [N, сторона, сторона, 3] — это не WD Tagger v3"
        )),
    }
}

/// Рейтинги модели: имя в словаре и подпись на плашке.
pub const RATINGS: [(&str, &str); 4] = [
    ("general", "Обычный"),
    ("sensitive", "Чувствительный"),
    ("questionable", "Сомнительный"),
    ("explicit", "18+"),
];

/// Имя рейтинга из запроса: `explicit`, `18+`, `обычный`… `None` — не рейтинг.
fn rating_name(word: &str) -> Option<&'static str> {
    let word = word.trim().to_lowercase();
    let found = match word.as_str() {
        "general" | "обычный" | "g" => "general",
        "sensitive" | "чувствительный" | "s" => "sensitive",
        "questionable" | "сомнительный" | "q" => "questionable",
        "explicit" | "откровенный" | "18+" | "e" => "explicit",
        _ => return None,
    };
    Some(found)
}

/// Тег в запросе — как имя тега для показа: `long_hair` → `long hair`, без регистра.
fn tag_pattern(word: &str) -> String {
    display_name(word.trim()).to_lowercase()
}

/// Слова запроса; `"long hair"` в кавычках — одно слово.
fn query_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Запрос по тегам: какие теги нужны, каких быть не должно, какие рейтинги показывать.
///
/// * `tag:cat` — нужен тег `cat`; `-tag:dog` — без тега `dog`; `long_hair` или `"long hair"` —
///   тег с пробелом; `*hair` — маска по имени тега;
/// * `rating:explicit` — только этот рейтинг, `-rating:18+` — без него (рейтинги: `general`,
///   `sensitive`, `questionable`, `explicit` или по-русски);
/// * в поиске по тегам и голые слова — теги (`cat outdoors -dog`), в фильтре папки они
///   остаются словами имени.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TagQuery {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Только эти рейтинги (пусто — любые).
    pub only_ratings: Vec<&'static str>,
    pub hide_ratings: Vec<&'static str>,
}

impl TagQuery {
    /// Разобрать запрос. `bare` — голые слова тоже теги. Возвращает запрос по тегам и
    /// остаток текста — то, что к тегам не относится.
    pub fn parse(text: &str, bare: bool) -> (TagQuery, String) {
        let mut query = TagQuery::default();
        let mut rest = Vec::new();
        for word in query_words(text) {
            let (negative, body) = match word.strip_prefix('-') {
                Some(body) if !body.is_empty() => (true, body),
                _ => (false, word.as_str()),
            };
            let lower = body.to_lowercase();
            if let Some(value) = lower.strip_prefix("rating:") {
                let ratings = value.split(',').filter_map(rating_name);
                if negative {
                    query.hide_ratings.extend(ratings);
                } else {
                    query.only_ratings.extend(ratings);
                }
                continue;
            }
            let tag = match lower.strip_prefix("tag:") {
                Some(tag) => tag.to_string(),
                None if bare => lower,
                None => {
                    rest.push(if word.contains(char::is_whitespace) {
                        format!("\"{word}\"")
                    } else {
                        word
                    });
                    continue;
                }
            };
            if tag.is_empty() {
                continue;
            }
            let pattern = tag_pattern(&tag);
            if negative { query.exclude.push(pattern) } else { query.include.push(pattern) }
        }
        query.only_ratings.dedup();
        query.hide_ratings.dedup();
        (query, rest.join(" "))
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty()
            && self.exclude.is_empty()
            && self.only_ratings.is_empty()
            && self.hide_ratings.is_empty()
    }

    /// Есть что искать: нужные теги или рейтинги (одни исключения — это всё подряд).
    pub fn selects(&self) -> bool {
        !self.include.is_empty() || !self.only_ratings.is_empty()
    }

    /// Подходит ли файл. `None` — тегов у файла нет (не картинка или ещё не посчитаны): такой
    /// не подходит, если нужен тег или рейтинг, и не мешает исключениям.
    pub fn matches(&self, shown: Option<&Shown>) -> bool {
        let Some(shown) = shown else {
            return self.include.is_empty() && self.only_ratings.is_empty();
        };
        let has = |pattern: &String| {
            shown.tags.iter().any(|tag| {
                let name = tag.name.to_lowercase();
                if pattern.contains(['*', '?']) {
                    let pattern: Vec<char> = pattern.chars().collect();
                    let name: Vec<char> = name.chars().collect();
                    crate::filter::glob_match(&pattern, &name)
                } else {
                    name == *pattern
                }
            })
        };
        let rating = shown.rating.as_ref().map(|r| r.name.as_str());
        self.include.iter().all(has)
            && !self.exclude.iter().any(has)
            && (self.only_ratings.is_empty()
                || rating.is_some_and(|r| self.only_ratings.contains(&r)))
            && !rating.is_some_and(|r| self.hide_ratings.contains(&r))
    }
}

impl TagQuery {
    /// Насколько файл подходит: сумма уверенности нужных тегов — для порядка выдачи.
    pub fn score(&self, shown: &Shown) -> f32 {
        self.include
            .iter()
            .map(|pattern| {
                shown
                    .tags
                    .iter()
                    .filter(|tag| {
                        let name = tag.name.to_lowercase();
                        if pattern.contains(['*', '?']) {
                            let p: Vec<char> = pattern.chars().collect();
                            crate::filter::glob_match(&p, &name.chars().collect::<Vec<_>>())
                        } else {
                            name == *pattern
                        }
                    })
                    .map(|tag| tag.score)
                    .fold(0.0, f32::max)
            })
            .sum()
    }
}

/// Тег для запроса: `long hair` → `long_hair` (без пробелов — одно слово).
pub fn query_token(name: &str) -> String {
    name.trim().replace(' ', "_")
}

/// Скрыт ли рейтинг в запросе (`-rating:…`).
pub fn rating_hidden(text: &str, rating: &str) -> bool {
    TagQuery::parse(text, false).0.hide_ratings.contains(&rating)
}

/// Скрыть рейтинг в запросе или вернуть: `-rating:explicit` дописывается или убирается.
pub fn toggle_rating(text: &str, rating: &str) -> String {
    let token = format!("-rating:{rating}");
    if rating_hidden(text, rating) {
        // Убрать все слова, которые прячут этот рейтинг (в любом написании).
        query_words(text)
            .into_iter()
            .filter(|word| {
                let lower = word.to_lowercase();
                !lower
                    .strip_prefix("-rating:")
                    .is_some_and(|v| v.split(',').filter_map(rating_name).any(|r| r == rating))
            })
            .map(
                |word| {
                    if word.contains(char::is_whitespace) { format!("\"{word}\"") } else { word }
                },
            )
            .collect::<Vec<_>>()
            .join(" ")
    } else if text.trim().is_empty() {
        token
    } else {
        format!("{} {token}", text.trim_end())
    }
}

const MAGIC: &[u8; 8] = b"MHFTAGS\n";
const VERSION: u32 = 1;

/// Теги файла и размер с временем, при которых они посчитаны.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub size: u64,
    pub modified: i64,
    pub tags: FileTags,
}

/// Посчитанные теги всех файлов одной модели.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagStore {
    /// Модель и словарь: `<репозиторий>|<отпечаток словаря>`. Другие — теги не подходят.
    pub model: String,
    records: HashMap<PathBuf, Record>,
}

impl TagStore {
    pub fn new(model: &str) -> TagStore {
        TagStore { model: model.to_string(), records: HashMap::new() }
    }

    /// Ключ модели для [`TagStore::model`].
    pub fn model_key(repo: &str, vocabulary: &Vocabulary) -> String {
        format!("{repo}|{:016x}", vocabulary.fingerprint())
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Теги, если файл не менялся с тех пор, как они посчитаны.
    pub fn fresh(&self, path: &Path, size: u64, modified: i64) -> Option<&FileTags> {
        self.records.get(path).filter(|r| r.size == size && r.modified == modified).map(|r| &r.tags)
    }

    pub fn insert(&mut self, path: PathBuf, record: Record) {
        self.records.insert(path, record);
    }

    pub fn clear(&mut self) {
        self.records.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PathBuf, &Record)> {
        self.records.iter()
    }

    pub fn write(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.records.len() * 160 + 64);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        put_str(&mut out, &self.model);
        out.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        // Порядок — по пути: одинаковые теги дают одинаковый файл.
        let mut paths: Vec<&PathBuf> = self.records.keys().collect();
        paths.sort();
        for path in paths {
            let record = &self.records[path];
            put_str(&mut out, &path.to_string_lossy());
            out.extend_from_slice(&record.size.to_le_bytes());
            out.extend_from_slice(&record.modified.to_le_bytes());
            let count = record.tags.scores.len().min(u16::MAX as usize);
            out.extend_from_slice(&(count as u16).to_le_bytes());
            for &(index, score) in &record.tags.scores[..count] {
                out.extend_from_slice(&index.to_le_bytes());
                out.push(score);
            }
        }
        let checksum = fnv(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }

    pub fn read(bytes: &[u8]) -> Result<TagStore, String> {
        let broken = || "файл тегов повреждён".to_string();
        if bytes.len() < MAGIC.len() + 8 || &bytes[..MAGIC.len()] != MAGIC {
            return Err("это не файл тегов MH Files".into());
        }
        let (payload, tail) = bytes.split_at(bytes.len() - 8);
        if fnv(payload) != u64::from_le_bytes(tail.try_into().map_err(|_| broken())?) {
            return Err(broken());
        }
        let mut reader = Reader { bytes: payload, at: MAGIC.len() };
        let version = reader.u32().ok_or_else(broken)?;
        if version != VERSION {
            return Err(format!("файл тегов версии {version} — теги посчитаются заново"));
        }
        let model = get_str(&mut reader).ok_or_else(broken)?;
        let count = reader.u32().ok_or_else(broken)? as usize;
        let mut records = HashMap::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            let path = PathBuf::from(get_str(&mut reader).ok_or_else(broken)?);
            let size = reader.u64().ok_or_else(broken)?;
            let modified = reader.i64().ok_or_else(broken)?;
            let tags = reader.u16().ok_or_else(broken)? as usize;
            let mut scores = Vec::with_capacity(tags);
            for _ in 0..tags {
                let index = reader.u32().ok_or_else(broken)?;
                let score = reader.take(1).ok_or_else(broken)?[0];
                scores.push((index, score));
            }
            records.insert(path, Record { size, modified, tags: FileTags { scores } });
        }
        if reader.at != payload.len() {
            return Err(broken());
        }
        Ok(TagStore { model, records })
    }
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn get_str(reader: &mut Reader) -> Option<String> {
    let len = reader.u32()? as usize;
    String::from_utf8(reader.take(len)?.to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "tag_id,name,category,count\n\
        9999999,general,9,1589178\n\
        9999998,sensitive,9,3994361\n\
        9999997,questionable,9,892496\n\
        9999996,explicit,9,706509\n\
        470575,1girl,0,4225150\n\
        212816,long_hair,0,3347498\n\
        1,^_^,0,1\n\
        2,cat,0,100\n\
        3,power_(chainsaw_man),4,9000\n";

    fn vocabulary() -> Vocabulary {
        Vocabulary::parse(CSV).unwrap()
    }

    #[test]
    fn parses_the_tag_list() {
        let v = vocabulary();
        assert_eq!(v.len(), 9);
        assert_eq!(v.name(5), "long hair");
        assert_eq!(v.name(6), "^_^", "смайлики не трогаются");
        assert_eq!(v.name(8), "power (chainsaw man)");
        assert_eq!(v.kind(0), TagKind::Rating);
        assert_eq!(v.kind(8), TagKind::Character);
        assert_eq!(v.kind(7), TagKind::General);
        // Windows-концы строк и пустая строка в конце.
        let crlf = CSV.replace('\n', "\r\n");
        assert_eq!(Vocabulary::parse(&crlf).unwrap(), v);
        assert!(Vocabulary::parse("tag_id,name\n1,a\n").is_err(), "нет категорий");
        assert!(Vocabulary::parse("tag_id,name,category,count\n").is_err(), "пусто");
        assert!(Vocabulary::parse("tag_id,name,category,count\n1,a,x,1\n").is_err());
        let other = Vocabulary::parse(&CSV.replace("cat,0", "dog,0")).unwrap();
        assert_ne!(other.fingerprint(), v.fingerprint(), "другой словарь — другой отпечаток");
    }

    #[test]
    fn keeps_confident_tags_and_one_rating() {
        let v = vocabulary();
        let scores = [0.9, 0.08, 0.01, 0.01, 0.99, 0.5, 0.1, 0.25, 0.97];
        let tags = FileTags::from_scores(&scores, &v).unwrap();
        let kept: Vec<u32> = tags.scores.iter().map(|&(i, _)| i).collect();
        assert_eq!(kept, vec![4, 8, 0, 5, 7], "по уверенности, ниже 0,2 — нет, рейтинг один");
        assert!(FileTags::from_scores(&scores[..3], &v).is_err(), "не тот размер выхода");
        let shown = show(&tags, &v, Thresholds::default());
        let names: Vec<&str> = shown.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["power (chainsaw man)", "1girl", "long hair"]);
        assert_eq!(shown.line, "power (chainsaw man), 1girl, long hair");
        assert_eq!(shown.rating.as_ref().map(|t| t.name.as_str()), Some("general"));
        assert!((shown.tags[1].score - 0.99).abs() < 0.01);
        // Порог ниже — виден и «cat», выше — только самые уверенные.
        let low = show(&tags, &v, Thresholds { general: 0.2, character: 0.99 });
        assert_eq!(low.line, "1girl, long hair, cat");
        // NaN от модели — не тег.
        let mut odd = scores;
        odd[7] = f32::NAN;
        assert!(!FileTags::from_scores(&odd, &v).unwrap().scores.iter().any(|&(i, _)| i == 7));
    }

    #[test]
    fn input_is_bgr_nhwc() {
        let rgb = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let tensor = input_tensor(&rgb, 2).unwrap();
        assert_eq!(&tensor[..6], &[30.0, 20.0, 10.0, 60.0, 50.0, 40.0]);
        assert!(input_tensor(&rgb, 3).is_none());
        assert_eq!(input_side(&[-1, 448, 448, 3]), Ok(448));
        assert_eq!(input_side(&[1, -1, -1, 3]), Ok(INPUT_SIDE));
        assert!(input_side(&[-1, 3, 448, 448]).is_err(), "NCHW — не WD Tagger v3");
    }

    #[test]
    fn store_round_trip() {
        let v = vocabulary();
        let key = TagStore::model_key(DEFAULT_MODEL, &v);
        let mut store = TagStore::new(&key);
        let tags =
            FileTags::from_scores(&[0.9, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.3, 0.0], &v).unwrap();
        let path = PathBuf::from("C:\\Фото\\кот.jpg");
        store.insert(path.clone(), Record { size: 10, modified: 5, tags: tags.clone() });
        store
            .insert("C:\\b.png".into(), Record { size: 1, modified: 2, tags: FileTags::default() });
        let bytes = store.write();
        let read = TagStore::read(&bytes).unwrap();
        assert_eq!(read, store);
        assert_eq!(read.fresh(&path, 10, 5), Some(&tags));
        assert_eq!(read.fresh(&path, 11, 5), None, "файл изменился");
        let mut bad = bytes.clone();
        bad[20] ^= 1;
        assert!(TagStore::read(&bad).is_err());
        assert!(TagStore::read(b"nonsense").is_err());
        assert_eq!(store.write(), bytes, "одинаковые теги — одинаковый файл");
    }

    #[test]
    fn tag_queries() {
        let v = vocabulary();
        let tags =
            FileTags::from_scores(&[0.9, 0.0, 0.0, 0.1, 0.99, 0.5, 0.1, 0.3, 0.97], &v).unwrap();
        let shown = show(&tags, &v, Thresholds { general: 0.25, character: 0.85 });
        let found = |text: &str, bare: bool| TagQuery::parse(text, bare).0.matches(Some(&shown));
        assert!(found("1girl long_hair", true));
        assert!(found("\"long hair\" -tag:dog", true));
        assert!(found("tag:*hair", false), "маска");
        assert!(!found("1girl -cat", true), "исключённый тег есть");
        assert!(!found("dog", true));
        assert!(found("POWER_(chainsaw_man)", true), "персонажи — тоже теги");
        assert!(found("rating:general", true) && !found("rating:explicit", true));
        assert!(found("-rating:18+", true) && !found("-rating:обычный", true));
        // Фильтр папки: голые слова — имя, теги — только с `tag:`.
        let (query, rest) = TagQuery::parse("отпуск tag:cat -rating:explicit *.jpg", false);
        assert_eq!(rest, "отпуск *.jpg");
        assert_eq!(query.include, vec!["cat"]);
        assert_eq!(query.hide_ratings, vec!["explicit"]);
        assert!(query.selects());
        // Без тегов: исключения не мешают, нужные теги — не найдены.
        assert!(TagQuery::parse("-rating:explicit -tag:dog", false).0.matches(None));
        assert!(!TagQuery::parse("tag:cat", false).0.matches(None));
        assert!(!TagQuery::parse("cat", false).0.selects());
        assert_eq!(query_token("long hair"), "long_hair");
    }

    #[test]
    fn rating_toggles() {
        let hidden = toggle_rating("cat", "explicit");
        assert_eq!(hidden, "cat -rating:explicit");
        assert!(rating_hidden(&hidden, "explicit") && !rating_hidden(&hidden, "general"));
        assert_eq!(toggle_rating(&hidden, "explicit"), "cat");
        assert_eq!(toggle_rating("-rating:18+ \"long hair\"", "explicit"), "\"long hair\"");
        assert_eq!(toggle_rating("", "questionable"), "-rating:questionable");
    }

    #[test]
    fn models_and_files() {
        assert_eq!(model_folder(DEFAULT_MODEL), "SmilingWolf--wd-eva02-large-tagger-v3");
        assert!(model_info("smilingwolf/WD-SWINV2-tagger-v3").is_some());
        let packages = runtime_packages();
        assert!(packages[0].0.ends_with("/microsoft.ml.onnxruntime.directml.1.24.4.nupkg"));
        let files: Vec<&str> =
            packages.iter().flat_map(|(_, f)| f.iter().map(|(_, d)| *d)).collect();
        assert!(
            RUNTIME_FILES.iter().all(|f| files.contains(f)),
            "все библиотеки откуда-то берутся"
        );
        assert_eq!(rating_label("explicit"), "откровенный (18+)");
        let missing =
            "failed to load from `C:\\x\\onnxruntime.dll`: LoadLibraryExW failed (os error 126)";
        assert!(explain_runtime_error(missing).contains(VC_REDIST));
        assert!(!explain_runtime_error("bad version").contains(VC_REDIST));
    }
}
