//! Векторы файлов в памяти и на диске.
//!
//! У файла — несколько частей: имя словами, куски текста, картинка, страницы PDF. Вектор
//! части хранится байтами (−127…127) с одним множителем на вектор: вчетверо меньше чисел
//! с плавающей точкой, а близость считается почти так же точно. Поиск — перебором: на
//! сотнях тысяч частей это десятки миллисекунд, и никакой отдельной базы не нужно.
//!
//! Снимок на диске — свой двоичный файл с версией и контрольной суммой, как у индекса дисков.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::DIMS;
use crate::index::snapshot::{Reader, fnv};

const MAGIC: &[u8; 8] = b"MHSEMAN\n";
const VERSION: u32 = 1;

/// Что за часть файла дала вектор.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartKind {
    /// Имя файла и папки словами.
    Name,
    /// Кусок текста.
    Text,
    /// Сама картинка.
    Image,
    /// Страница документа картинкой.
    Page,
    /// Начало звукозаписи.
    Audio,
}

impl PartKind {
    fn code(self) -> u8 {
        match self {
            PartKind::Name => 0,
            PartKind::Text => 1,
            PartKind::Image => 2,
            PartKind::Page => 3,
            PartKind::Audio => 4,
        }
    }

    fn from_code(code: u8) -> Option<PartKind> {
        Some(match code {
            0 => PartKind::Name,
            1 => PartKind::Text,
            2 => PartKind::Image,
            3 => PartKind::Page,
            4 => PartKind::Audio,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub kind: PartKind,
    /// Номер куска текста или страницы, с нуля.
    pub number: u16,
    /// Начало куска текста — подпись найденного.
    pub snippet: String,
    scale: f32,
    vector: Box<[i8]>,
}

impl Part {
    /// Часть с вектором длины [`DIMS`] (уже нормированным).
    pub fn new(kind: PartKind, number: u16, snippet: String, vector: &[f32]) -> Part {
        let (scale, vector) = quantize(vector);
        Part { kind, number, snippet, scale, vector }
    }

    /// Близость к запросу (косинус: оба вектора длины 1).
    pub fn score(&self, query: &[f32]) -> f32 {
        let dot: f32 = self.vector.iter().zip(query).map(|(&v, &q)| f32::from(v) * q).sum();
        dot * self.scale
    }
}

/// Байты вместо чисел: множитель — наибольшее по модулю число / 127.
fn quantize(vector: &[f32]) -> (f32, Box<[i8]>) {
    let mut values = vec![0i8; DIMS];
    let max = vector.iter().take(DIMS).fold(0f32, |m, v| m.max(v.abs()));
    if max == 0.0 || !max.is_finite() {
        return (0.0, values.into_boxed_slice());
    }
    let scale = max / 127.0;
    for (slot, v) in values.iter_mut().zip(vector) {
        *slot = (v / scale).round().clamp(-127.0, 127.0) as i8;
    }
    (scale, values.into_boxed_slice())
}

/// Проиндексированный файл.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub size: u64,
    /// Время изменения, секунды Unix.
    pub modified: i64,
    pub parts: Vec<Part>,
}

/// Найденный файл.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: PathBuf,
    pub score: f32,
    /// Чем файл подошёл лучше всего.
    pub kind: PartKind,
    pub number: u16,
    pub snippet: String,
}

impl Hit {
    /// Почему найдено — для подписи.
    pub fn reason(&self) -> String {
        match self.kind {
            PartKind::Name => "по имени".to_string(),
            PartKind::Image => "по изображению".to_string(),
            PartKind::Audio => "по звуку".to_string(),
            PartKind::Page => format!("по странице {}", self.number + 1),
            PartKind::Text if self.snippet.is_empty() => "по тексту".to_string(),
            PartKind::Text => format!("«{}»", self.snippet),
        }
    }
}

/// Найденное хуже этого не показывается: дальше — случайные совпадения.
pub const MIN_SCORE: f32 = 0.2;
/// Прибавка, если все слова запроса есть в имени файла: точное совпадение имени важнее
/// похожего смысла.
pub const NAME_BONUS: f32 = 0.08;

/// Все векторы.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Store {
    /// Чем считались векторы: файл модели, подсказки, длина. Другая модель — другой индекс.
    pub model: String,
    docs: HashMap<PathBuf, Doc>,
}

impl Store {
    pub fn new(model: &str) -> Store {
        Store { model: model.to_string(), docs: HashMap::new() }
    }

    /// Файлов.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Векторов всего.
    pub fn parts(&self) -> usize {
        self.docs.values().map(|d| d.parts.len()).sum()
    }

    pub fn get(&self, path: &Path) -> Option<&Doc> {
        self.docs.get(path)
    }

    /// Файл уже в индексе с тем же размером и временем — пересчитывать не нужно.
    pub fn fresh(&self, path: &Path, size: u64, modified: i64) -> bool {
        self.docs.get(path).is_some_and(|d| d.size == size && d.modified == modified)
    }

    pub fn insert(&mut self, path: PathBuf, doc: Doc) {
        self.docs.insert(path, doc);
    }

    pub fn remove(&mut self, path: &Path) -> bool {
        self.docs.remove(path).is_some()
    }

    /// Оставить только файлы, для которых `keep` истинно; сколько убрано.
    pub fn retain(&mut self, mut keep: impl FnMut(&Path) -> bool) -> usize {
        let before = self.docs.len();
        self.docs.retain(|path, _| keep(path));
        before - self.docs.len()
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.docs.keys().map(PathBuf::as_path)
    }

    /// Лучшие `limit` файлов для вектора запроса. `words` — слова запроса в нижнем регистре
    /// (для прибавки за имя), `keep` — фильтр (категория).
    pub fn search(
        &self,
        query: &[f32],
        words: &[String],
        limit: usize,
        keep: impl Fn(&Path) -> bool + Sync,
    ) -> Vec<Hit> {
        if query.len() < DIMS || limit == 0 {
            return Vec::new();
        }
        let docs: Vec<(&PathBuf, &Doc)> = self.docs.iter().collect();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        // Мелкий индекс — в своём потоке: заводить потоки дольше, чем считать.
        let per = if docs.len() < 4096 { docs.len().max(1) } else { docs.len().div_ceil(threads) };
        let best = |slice: &[(&PathBuf, &Doc)]| -> Vec<Hit> {
            let mut hits = Vec::new();
            for (path, doc) in slice {
                let Some((part, mut score)) = doc
                    .parts
                    .iter()
                    .map(|p| (p, p.score(query)))
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                else {
                    continue;
                };
                if !words.is_empty() && name_has(path, words) {
                    score += NAME_BONUS;
                }
                if score < MIN_SCORE || !keep(path) {
                    continue;
                }
                hits.push(Hit {
                    path: (*path).clone(),
                    score,
                    kind: part.kind,
                    number: part.number,
                    snippet: part.snippet.clone(),
                });
            }
            top(hits, limit)
        };
        let mut hits: Vec<Hit> = if docs.len() <= per {
            best(&docs)
        } else {
            std::thread::scope(|scope| {
                let jobs: Vec<_> =
                    docs.chunks(per).map(|slice| scope.spawn(|| best(slice))).collect();
                jobs.into_iter().flat_map(|job| job.join().unwrap_or_default()).collect()
            })
        };
        hits = top(hits, limit);
        hits
    }

    /// Снимок для записи на диск.
    pub fn write(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.parts() * (DIMS + 16) + self.docs.len() * 64 + 64);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        put_str(&mut out, &self.model);
        out.extend_from_slice(&(DIMS as u32).to_le_bytes());
        out.extend_from_slice(&(self.docs.len() as u32).to_le_bytes());
        // Порядок — по пути: одинаковый индекс даёт одинаковый файл.
        let mut paths: Vec<&PathBuf> = self.docs.keys().collect();
        paths.sort();
        for path in paths {
            let doc = &self.docs[path];
            put_str(&mut out, &path.to_string_lossy());
            out.extend_from_slice(&doc.size.to_le_bytes());
            out.extend_from_slice(&doc.modified.to_le_bytes());
            out.extend_from_slice(&(doc.parts.len() as u16).to_le_bytes());
            for part in &doc.parts {
                out.push(part.kind.code());
                out.extend_from_slice(&part.number.to_le_bytes());
                let snippet = part.snippet.as_bytes();
                let len = snippet.len().min(u16::MAX as usize);
                // Обрезка по байтам могла рассечь символ — его не пишем.
                let len = (0..=len).rev().find(|&n| part.snippet.is_char_boundary(n)).unwrap_or(0);
                out.extend_from_slice(&(len as u16).to_le_bytes());
                out.extend_from_slice(&snippet[..len]);
                out.extend_from_slice(&part.scale.to_le_bytes());
                out.extend(part.vector.iter().map(|&v| v as u8));
            }
        }
        let checksum = fnv(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }

    /// Прочитать снимок. Ошибка — текстом: файл чужой, повреждён или другой версии.
    pub fn read(bytes: &[u8]) -> Result<Store, String> {
        let broken = || "индекс поиска по смыслу повреждён".to_string();
        if bytes.len() < MAGIC.len() + 8 || &bytes[..MAGIC.len()] != MAGIC {
            return Err("это не индекс поиска по смыслу MH Files".into());
        }
        let (payload, tail) = bytes.split_at(bytes.len() - 8);
        if fnv(payload) != u64::from_le_bytes(tail.try_into().map_err(|_| broken())?) {
            return Err(broken());
        }
        let mut reader = Reader { bytes: payload, at: MAGIC.len() };
        let version = reader.u32().ok_or_else(broken)?;
        if version != VERSION {
            return Err(format!(
                "индекс поиска по смыслу версии {version} — будет построен заново"
            ));
        }
        let model = get_str(&mut reader).ok_or_else(broken)?;
        let dims = reader.u32().ok_or_else(broken)? as usize;
        if dims != DIMS {
            return Err("индекс поиска по смыслу другой длины — будет построен заново".into());
        }
        let count = reader.u32().ok_or_else(broken)? as usize;
        let mut docs = HashMap::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            let path = PathBuf::from(get_str(&mut reader).ok_or_else(broken)?);
            let size = reader.u64().ok_or_else(broken)?;
            let modified = reader.i64().ok_or_else(broken)?;
            let parts_count = reader.u16().ok_or_else(broken)? as usize;
            let mut parts = Vec::with_capacity(parts_count);
            for _ in 0..parts_count {
                let kind = PartKind::from_code(reader.take(1).ok_or_else(broken)?[0])
                    .ok_or_else(broken)?;
                let number = reader.u16().ok_or_else(broken)?;
                let len = reader.u16().ok_or_else(broken)? as usize;
                let snippet = std::str::from_utf8(reader.take(len).ok_or_else(broken)?)
                    .map_err(|_| broken())?
                    .to_string();
                let scale = f32::from_le_bytes(
                    reader.take(4).ok_or_else(broken)?.try_into().map_err(|_| broken())?,
                );
                let vector: Box<[i8]> =
                    reader.take(DIMS).ok_or_else(broken)?.iter().map(|&b| b as i8).collect();
                parts.push(Part { kind, number, snippet, scale, vector });
            }
            docs.insert(path, Doc { size, modified, parts });
        }
        if reader.at != payload.len() {
            return Err(broken());
        }
        Ok(Store { model, docs })
    }
}

/// Все слова запроса есть в имени файла (без учёта регистра).
fn name_has(path: &Path, words: &[String]) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    words.iter().all(|word| name.contains(word.as_str()))
}

/// Лучшие `limit`, лучшие первыми.
fn top(mut hits: Vec<Hit>, limit: usize) -> Vec<Hit> {
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    hits.truncate(limit);
    hits
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

    /// Единичный вектор вдоль оси `axis` с примесью `mix` по оси `other`.
    fn unit(axis: usize, other: usize, mix: f32) -> Vec<f32> {
        let mut v = vec![0.0; DIMS];
        v[axis] = 1.0;
        v[other] += mix;
        super::super::prepare(&v).unwrap()
    }

    fn doc(parts: Vec<Part>) -> Doc {
        Doc { size: 10, modified: 100, parts }
    }

    fn sample() -> Store {
        let mut store = Store::new("model|v1");
        store.insert(
            "/photos/cat.jpg".into(),
            doc(vec![
                Part::new(PartKind::Name, 0, String::new(), &unit(5, 6, 0.0)),
                Part::new(PartKind::Image, 0, String::new(), &unit(1, 2, 0.1)),
            ]),
        );
        store.insert(
            "/docs/lease.docx".into(),
            doc(vec![Part::new(PartKind::Text, 1, "Договор аренды".into(), &unit(2, 1, 0.3))]),
        );
        store.insert(
            "/docs/empty.bin".into(),
            doc(Vec::new()), // не вышло прочитать: без частей, но запомнен, чтобы не повторять.
        );
        store
    }

    #[test]
    fn quantized_scores_are_close_to_exact() {
        let a = unit(1, 2, 0.4);
        let b = unit(1, 3, 0.7);
        let exact: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        let part = Part::new(PartKind::Text, 0, String::new(), &a);
        assert!((part.score(&b) - exact).abs() < 0.01, "{} vs {exact}", part.score(&b));
        let zero = Part::new(PartKind::Text, 0, String::new(), &vec![0.0; DIMS]);
        assert_eq!(zero.score(&b), 0.0);
    }

    #[test]
    fn search_ranks_by_best_part() {
        let store = sample();
        assert_eq!((store.len(), store.parts()), (3, 3));
        let hits = store.search(&unit(1, 2, 0.0), &[], 10, |_| true);
        let paths: Vec<_> = hits.iter().map(|h| h.path.to_string_lossy().into_owned()).collect();
        assert_eq!(paths, ["/photos/cat.jpg", "/docs/lease.docx"]);
        assert_eq!(hits[0].kind, PartKind::Image);
        assert_eq!(hits[0].reason(), "по изображению");
        assert_eq!(hits[1].reason(), "«Договор аренды»");
        // Ось, которой нет ни у одного файла, — ничего не похоже.
        assert!(store.search(&unit(100, 101, 0.0), &[], 10, |_| true).is_empty());
        assert_eq!(store.search(&unit(1, 2, 0.0), &[], 1, |_| true).len(), 1);
        let only_docs = store.search(&unit(1, 2, 0.0), &[], 10, |p| p.starts_with("/docs"));
        assert_eq!(only_docs.len(), 1);
        assert!(store.search(&[0.5; 3], &[], 10, |_| true).is_empty(), "негодный запрос");
    }

    #[test]
    fn name_match_lifts_a_file() {
        let store = sample();
        let query = unit(2, 1, 0.5);
        let plain = store.search(&query, &[], 10, |_| true);
        assert_eq!(plain[0].path, Path::new("/docs/lease.docx"));
        let lifted = store.search(&query, &["cat".to_string()], 10, |_| true);
        let cat = lifted.iter().find(|h| h.path == Path::new("/photos/cat.jpg")).unwrap();
        let before = plain.iter().find(|h| h.path == Path::new("/photos/cat.jpg")).unwrap();
        assert!((cat.score - before.score - NAME_BONUS).abs() < 1e-5);
    }

    #[test]
    fn search_in_parallel_matches_single_thread() {
        let mut store = Store::new("m");
        for i in 0..5000 {
            let part = Part::new(
                PartKind::Text,
                0,
                String::new(),
                &unit(i % 7, 7 + i % 5, 0.05 * (i % 11) as f32),
            );
            store.insert(PathBuf::from(format!("/f/{i:05}.txt")), doc(vec![part]));
        }
        let hits = store.search(&unit(3, 9, 0.2), &[], 50, |_| true);
        assert_eq!(hits.len(), 50);
        assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
        // Тот же ответ при поиске по одному файлу за раз.
        let best = store
            .paths()
            .map(|p| (p.to_path_buf(), store.get(p).unwrap().parts[0].score(&unit(3, 9, 0.2))))
            .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .unwrap();
        assert_eq!(hits[0].path, best.0);
    }

    #[test]
    fn freshness_and_removal() {
        let mut store = sample();
        assert!(store.fresh(Path::new("/docs/lease.docx"), 10, 100));
        assert!(!store.fresh(Path::new("/docs/lease.docx"), 11, 100));
        assert!(!store.fresh(Path::new("/nope"), 10, 100));
        assert!(store.remove(Path::new("/docs/lease.docx")));
        assert!(!store.remove(Path::new("/docs/lease.docx")));
        assert_eq!(store.retain(|p| p.starts_with("/photos")), 1);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn snapshot_round_trip_and_damage() {
        let mut store = sample();
        // Длинная подпись с многобайтными символами обрезается по границе символа.
        let long = "ж".repeat(40_000);
        store.insert(
            "/docs/long.txt".into(),
            doc(vec![Part::new(PartKind::Page, 3, long, &unit(4, 5, 0.2))]),
        );
        let bytes = store.write();
        assert_eq!(bytes, store.write(), "одинаковый индекс — одинаковые байты");
        let loaded = Store::read(&bytes).unwrap();
        assert_eq!(loaded.model, "model|v1");
        assert_eq!(loaded.len(), store.len());
        assert_eq!(
            loaded.get(Path::new("/docs/lease.docx")),
            store.get(Path::new("/docs/lease.docx"))
        );
        let page = &loaded.get(Path::new("/docs/long.txt")).unwrap().parts[0];
        assert_eq!((page.kind, page.number), (PartKind::Page, 3));
        assert!(page.snippet.len() <= u16::MAX as usize && page.snippet.chars().all(|c| c == 'ж'));
        let mut broken = bytes.clone();
        let middle = broken.len() / 2;
        broken[middle] ^= 0x55;
        assert!(Store::read(&broken).is_err());
        assert!(Store::read(&bytes[..bytes.len() - 1]).is_err());
        assert!(Store::read(b"MHINDEX\nxxxxxxxxxxxx").is_err());
        assert!(Store::read(&Store::new("").write()).unwrap().is_empty());
    }
}
