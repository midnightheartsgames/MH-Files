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
    /// Имя файла и папки словами. Такие векторы были в индексах 1.2.0–1.2.3 и при поиске
    /// не учитываются; так помечается и файл, найденный по словам имени.
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
    /// Сходство с обычными запросами ([`super::bank`]): среднее и разброс. Не хранится на
    /// диске — считается, когда мерка известна ([`Store::set_bank`]).
    typical: Option<(f32, f32)>,
}

impl Part {
    /// Часть с вектором длины [`DIMS`] (уже нормированным).
    pub fn new(kind: PartKind, number: u16, snippet: String, vector: &[f32]) -> Part {
        let (scale, vector) = quantize(vector);
        Part { kind, number, snippet, scale, vector, typical: None }
    }

    fn measure(&mut self, bank: &[Vec<f32>]) {
        self.typical = (!bank.is_empty()).then(|| {
            let mut stats = Stats::default();
            for query in bank {
                stats.add(self.score(query));
            }
            let mean = stats.mean();
            let variance = (stats.squares / stats.n - mean * mean).max(0.0);
            (mean as f32, (variance.sqrt() as f32).max(MIN_TYPICAL_SPREAD))
        });
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
    /// Сходство с запросом (косинус) лучшей части — для показа.
    pub score: f32,
    /// Насколько эта часть выделяется, в отклонениях (с прибавкой за слова в имени) — по
    /// нему порядок.
    pub rank: f32,
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

/// Сырое сходство у модели не говорит, «похоже» это или нет: у EmbeddingGemma 2 любой запрос
/// похож на любой файл на 0,5–0,7. Поэтому часть оценивается тем, насколько запрос подходит
/// ей лучше обычных запросов ([`super::bank`]): (сходство − среднее) / разброс. Показывается
/// только подходящее не меньше чем на `min_rank` отклонений (настройка «Отбор»; по умолчанию —
/// столько).
pub const MIN_RANK: f32 = 3.0;
/// Разброс сходства части с обычными запросами — не меньше этого (у настоящих частей —
/// 0,02–0,03).
const MIN_TYPICAL_SPREAD: f32 = 0.01;
/// Вес совпадения слов запроса со словами имени файла (и его папки), рядом с выделением по
/// смыслу: имя, где есть все слова, прибавляет столько. Файл, в имени которого хотя бы
/// половина слов запроса, виден при любом отборе.
pub const WORD_WEIGHT: f32 = 2.0;
/// Пока мерки нет (модель только запускается), часть оценивается выделением среди частей
/// своего вида (тексты, картинки, звук): сходство минус среднее вида, делённое на общий
/// разброс, — так, как ищет DigUp. Разброс считается не меньше этого: в индексе из похожих
/// файлов любое отличие в сотых «выделялось» бы на много отклонений.
const MIN_SPREAD: f32 = 0.02;
/// В виде частей меньше — его среднее берётся по всем частям; всего меньше — порог не
/// применяется (индекс слишком мал для статистики), только порядок.
const MIN_STATS: usize = 20;
/// Видов частей по смыслу: тексты, картинки (и страницы), звук. Имена файлов векторами
/// больше не считаются: «Gato», «Chimera» и прочие слова имён похожи на запросы случайно —
/// имя сравнивается по словам ([`WORD_WEIGHT`]).
const MODALITIES: usize = 3;

fn modality(kind: PartKind) -> Option<usize> {
    match kind {
        PartKind::Name => None,
        PartKind::Text => Some(0),
        PartKind::Image | PartKind::Page => Some(1),
        PartKind::Audio => Some(2),
    }
}

/// Сумма и сумма квадратов сходства частей.
#[derive(Debug, Clone, Copy, Default)]
struct Stats {
    n: f64,
    sum: f64,
    squares: f64,
}

impl Stats {
    fn add(&mut self, score: f32) {
        let score = f64::from(score);
        self.n += 1.0;
        self.sum += score;
        self.squares += score * score;
    }

    fn merge(&mut self, other: &Stats) {
        self.n += other.n;
        self.sum += other.sum;
        self.squares += other.squares;
    }

    fn mean(&self) -> f64 {
        if self.n == 0.0 { 0.0 } else { self.sum / self.n }
    }
}

/// Как превратить сходство части в выделение: средние видов и общий разброс.
struct Scale {
    means: [f32; MODALITIES],
    /// Среднее выделения после вычитания средних видов (не ноль, если у мелкого вида
    /// вычиталось общее среднее).
    center: f32,
    spread: f32,
}

impl Scale {
    fn new(stats: &[Stats; MODALITIES]) -> Scale {
        let mut all = Stats::default();
        for s in stats {
            all.merge(s);
        }
        let means = stats
            .map(|s| if s.n >= MIN_STATS as f64 { s.mean() as f32 } else { all.mean() as f32 });
        // Сумма и сумма квадратов (сходство − среднее вида) — из сумм каждого вида.
        let (mut sum, mut squares) = (0.0, 0.0);
        for (s, &mean) in stats.iter().zip(&means) {
            let mean = f64::from(mean);
            sum += s.sum - s.n * mean;
            squares += s.squares - 2.0 * mean * s.sum + s.n * mean * mean;
        }
        let n = all.n.max(1.0);
        let center = sum / n;
        let variance = (squares / n - center * center).max(0.0);
        Scale { means, center: center as f32, spread: (variance.sqrt() as f32).max(MIN_SPREAD) }
    }

    fn rank(&self, modality: usize, score: f32) -> f32 {
        (score - self.means[modality] - self.center) / self.spread
    }
}

/// Все векторы.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Store {
    /// Чем считались векторы: файл модели, подсказки, длина. Другая модель — другой индекс.
    pub model: String,
    docs: HashMap<PathBuf, Doc>,
    /// Векторы обычных запросов ([`super::bank`]); пусто — мерка ещё не посчитана.
    bank: Vec<Vec<f32>>,
}

impl Store {
    pub fn new(model: &str) -> Store {
        Store { model: model.to_string(), docs: HashMap::new(), bank: Vec::new() }
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

    pub fn insert(&mut self, path: PathBuf, mut doc: Doc) {
        for part in &mut doc.parts {
            part.measure(&self.bank);
        }
        self.docs.insert(path, doc);
    }

    /// Мерка посчитана ([`Self::set_bank`]).
    pub fn has_bank(&self) -> bool {
        !self.bank.is_empty()
    }

    /// Векторы обычных запросов: по ним меряется каждая часть (по потокам — частей бывают
    /// сотни тысяч).
    pub fn set_bank(&mut self, bank: Vec<Vec<f32>>) {
        self.bank = bank;
        let bank = &self.bank;
        let mut docs: Vec<&mut Doc> = self.docs.values_mut().collect();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        let per = docs.len().div_ceil(threads).max(256);
        std::thread::scope(|scope| {
            for slice in docs.chunks_mut(per) {
                scope.spawn(move || {
                    for doc in slice {
                        for part in &mut doc.parts {
                            part.measure(bank);
                        }
                    }
                });
            }
        });
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

    /// Лучшие `limit` файлов для вектора запроса. `words` — слова запроса
    /// ([`super::query_words`]) для сравнения с именами, `min_rank` — отбор в отклонениях
    /// ([`MIN_RANK`]), `keep` — фильтр (категория). Порядок — по [`Hit::rank`].
    pub fn search(
        &self,
        query: &[f32],
        words: &[String],
        min_rank: f32,
        limit: usize,
        keep: impl Fn(&Path) -> bool + Sync,
    ) -> Vec<Hit> {
        if query.len() < DIMS || limit == 0 {
            return Vec::new();
        }
        let docs: Vec<(&PathBuf, &Doc)> = self.docs.iter().collect();
        // Первый проход — сходство всех частей и статистика по видам; по потокам.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        // Мелкий индекс — в своём потоке: заводить потоки дольше, чем считать.
        let per = if docs.len() < 4096 { docs.len().max(1) } else { docs.len().div_ceil(threads) };
        let score = |slice: &[(&PathBuf, &Doc)]| -> (Vec<Vec<f32>>, [Stats; MODALITIES]) {
            let mut stats = [Stats::default(); MODALITIES];
            let scores = slice
                .iter()
                .map(|(_, doc)| {
                    doc.parts
                        .iter()
                        .map(|part| match modality(part.kind) {
                            Some(m) => {
                                let score = part.score(query);
                                stats[m].add(score);
                                score
                            }
                            None => f32::NEG_INFINITY,
                        })
                        .collect()
                })
                .collect();
            (scores, stats)
        };
        let chunks: Vec<(Vec<Vec<f32>>, [Stats; MODALITIES])> = if docs.len() <= per {
            vec![score(&docs)]
        } else {
            std::thread::scope(|scope| {
                let jobs: Vec<_> =
                    docs.chunks(per).map(|slice| scope.spawn(|| score(slice))).collect();
                jobs.into_iter().map(|job| job.join().unwrap_or_default()).collect()
            })
        };
        let mut stats = [Stats::default(); MODALITIES];
        for (_, part) in &chunks {
            for (total, part) in stats.iter_mut().zip(part) {
                total.merge(part);
            }
        }
        let enough = stats.iter().map(|s| s.n).sum::<f64>() >= MIN_STATS as f64;
        let scale = Scale::new(&stats);
        // Второй проход — лучшая часть каждого файла и слова имени.
        let mut hits = Vec::new();
        let scores = chunks.into_iter().flat_map(|(scores, _)| scores);
        for ((path, doc), scores) in docs.iter().zip(scores) {
            let best = doc.parts.iter().zip(&scores).filter_map(|(part, &score)| {
                let rank = match part.typical {
                    Some((mean, spread)) => (score - mean) / spread,
                    None => scale.rank(modality(part.kind)?, score),
                };
                modality(part.kind).map(|_| (part, score, rank))
            });
            let best = best.max_by(|a, b| a.2.total_cmp(&b.2));
            let meaning = best.map_or(f32::NEG_INFINITY, |(_, _, rank)| rank);
            // Без мерки и статистики (крошечный индекс) — только порядок.
            let enough = enough || best.is_some_and(|(part, _, _)| part.typical.is_some());
            let coverage = if words.is_empty() {
                0.0
            } else {
                super::name_coverage(&super::name_tokens(path), words)
            };
            let by_meaning = best.is_some() && (!enough || meaning >= min_rank);
            if !(by_meaning || coverage >= 0.5) || !keep(path) {
                continue;
            }
            let rank = if by_meaning { meaning } else { 0.0 } + WORD_WEIGHT * coverage.powf(1.5);
            let hit = match best {
                Some((part, score, _)) if by_meaning => Hit {
                    path: (*path).clone(),
                    score,
                    rank,
                    kind: part.kind,
                    number: part.number,
                    snippet: part.snippet.clone(),
                },
                _ => Hit {
                    path: (*path).clone(),
                    score: best.map_or(0.0, |(_, score, _)| score),
                    rank,
                    kind: PartKind::Name,
                    number: 0,
                    snippet: String::new(),
                },
            };
            hits.push(hit);
        }
        top(hits, limit)
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
                // Векторы имён из индексов 1.2.0–1.2.3 при поиске не нужны.
                if kind != PartKind::Name {
                    parts.push(Part { kind, number, snippet, scale, vector, typical: None });
                }
            }
            docs.insert(path, Doc { size, modified, parts });
        }
        if reader.at != payload.len() {
            return Err(broken());
        }
        Ok(Store { model, docs, bank: Vec::new() })
    }
}

/// Лучшие `limit`, лучшие первыми.
fn top(mut hits: Vec<Hit>, limit: usize) -> Vec<Hit> {
    hits.sort_by(|a, b| b.rank.total_cmp(&a.rank).then_with(|| a.path.cmp(&b.path)));
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

    /// Вектор со сходством `cosine` с осью 0 (запросом); остальное — вдоль своей оси `axis`.
    fn at(cosine: f32, axis: usize) -> Vec<f32> {
        let mut v = vec![0.0; DIMS];
        v[0] = cosine;
        v[axis] = (1.0 - cosine * cosine).sqrt();
        v
    }

    /// Запрос — ось 0.
    fn query() -> Vec<f32> {
        at(1.0, 1)
    }

    /// Фон: по 60 имён (векторы из индексов 1.2.0–1.2.3: при поиске не учитываются), текстов
    /// (около 0,45) и картинок (около 0,2: текст с картинкой сходится слабее), плюс три файла.
    /// Мерки нет — оценка по выделению среди своего вида.
    fn sample() -> Store {
        let mut store = Store::new("model|v1");
        for i in 0..60 {
            let jitter = (i % 7) as f32 * 0.005;
            store.insert(
                format!("/bg/name{i:02}.lnk").into(),
                doc(vec![Part::new(PartKind::Name, 0, String::new(), &at(0.28 + jitter, 10 + i))]),
            );
            store.insert(
                format!("/bg/text{i:02}.txt").into(),
                doc(vec![Part::new(PartKind::Text, 0, String::new(), &at(0.43 + jitter, 80 + i))]),
            );
            store.insert(
                format!("/bg/image{i:02}.jpg").into(),
                doc(vec![Part::new(
                    PartKind::Image,
                    0,
                    String::new(),
                    &at(0.18 + jitter, 150 + i),
                )]),
            );
        }
        // Кошка на картинке: сырое сходство (0,32) ниже любого фонового текста (0,43–0,46),
        // но среди картинок она явно выделяется.
        store.insert(
            "/photos/cat.jpg".into(),
            doc(vec![
                Part::new(PartKind::Name, 0, String::new(), &at(0.28, 300)),
                Part::new(PartKind::Image, 0, String::new(), &at(0.32, 301)),
            ]),
        );
        store.insert(
            "/docs/lease.docx".into(),
            doc(vec![Part::new(PartKind::Text, 1, "Договор аренды".into(), &at(0.6, 302))]),
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
    fn standouts_of_each_kind_and_no_background() {
        let store = sample();
        assert_eq!((store.len(), store.parts()), (183, 183));
        let hits = store.search(&query(), &[], MIN_RANK, 300, |_| true);
        let mut paths: Vec<_> =
            hits.iter().map(|h| h.path.to_string_lossy().into_owned()).collect();
        paths.sort();
        // Фон не проходит, хотя сырое сходство у фоновых текстов выше, чем у кошки.
        assert_eq!(paths, ["/docs/lease.docx", "/photos/cat.jpg"], "{hits:?}");
        let cat = hits.iter().find(|h| h.kind == PartKind::Image).unwrap();
        assert!((cat.score - 0.32).abs() < 0.01, "показывается сырое сходство");
        assert_eq!(cat.reason(), "по изображению");
        let lease = hits.iter().find(|h| h.kind == PartKind::Text).unwrap();
        assert_eq!(lease.reason(), "«Договор аренды»");
        assert_eq!(store.search(&query(), &[], MIN_RANK, 1, |_| true).len(), 1);
        let only_docs = store.search(&query(), &[], MIN_RANK, 10, |p| p.starts_with("/docs"));
        assert_eq!(only_docs.len(), 1);
        assert!(store.search(&[0.5; 3], &[], MIN_RANK, 10, |_| true).is_empty(), "негодный запрос");
        // Мягкий отбор пропускает и фон, строгий — ничего лишнего.
        assert!(store.search(&query(), &[], 0.5, 300, |_| true).len() > 2);
        assert_eq!(store.search(&query(), &[], 100.0, 300, |_| true).len(), 0);
    }

    #[test]
    fn nothing_stands_out_nothing_found() {
        let mut store = Store::new("m");
        for i in 0..100 {
            let part = Part::new(PartKind::Text, 0, String::new(), &at(0.3, 10 + i));
            store.insert(PathBuf::from(format!("/f/{i:03}")), doc(vec![part]));
        }
        assert!(store.search(&query(), &[], MIN_RANK, 300, |_| true).is_empty());
    }

    #[test]
    fn tiny_index_is_ordered_without_cutoff() {
        let mut store = Store::new("m");
        for (i, cosine) in [0.2f32, 0.5, 0.3].into_iter().enumerate() {
            let part = Part::new(PartKind::Text, 0, String::new(), &at(cosine, 10 + i));
            store.insert(PathBuf::from(format!("/f/{i}")), doc(vec![part]));
        }
        let hits = store.search(&query(), &[], MIN_RANK, 10, |_| true);
        let paths: Vec<_> = hits.iter().map(|h| h.path.to_string_lossy().into_owned()).collect();
        assert_eq!(paths, ["/f/1", "/f/2", "/f/0"]);
    }

    #[test]
    fn words_of_the_name_find_a_file() {
        let mut store = sample();
        store.insert("/bg/Кошки на даче.lnk".into(), doc(Vec::new()));
        store.insert("/bg/Ночной кошмар.png".into(), doc(Vec::new()));
        let plain = store.search(&query(), &[], MIN_RANK, 300, |_| true);
        assert!(plain.iter().all(|h| !h.path.starts_with("/bg")));
        let words = super::super::query_words("кошка на диване");
        let found = store.search(&query(), &words, 100.0, 300, |_| true);
        // Половина слов в имени — виден при любом отборе; «кошмар» — не «кошка».
        let names: Vec<_> = found.iter().map(|h| h.path.to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["/bg/Кошки на даче.lnk"]);
        assert_eq!(found[0].reason(), "по имени");
        // Найденное по смыслу и по имени — выше такой же картинки, найденной только по смыслу.
        store.insert(
            "/photos/кошка.jpg".into(),
            doc(vec![Part::new(PartKind::Image, 0, String::new(), &at(0.32, 401))]),
        );
        let hits = store.search(&query(), &words, MIN_RANK, 300, |_| true);
        let place = |p: &str| hits.iter().position(|h| h.path == Path::new(p)).unwrap();
        assert!(place("/photos/кошка.jpg") < place("/photos/cat.jpg"), "{hits:?}");
        assert_eq!(hits[place("/photos/кошка.jpg")].kind, PartKind::Image);
    }

    /// Мерка обычными запросами: картинка, похожая на любой запрос (как скриншот с текстом),
    /// не находится, хотя сырое сходство у неё выше, чем у настоящей находки.
    #[test]
    fn parts_that_match_any_query_are_not_found() {
        // Общее направление «похоже на всё» — ось 1: в нём и запрос, и обычные запросы.
        let blend = |axis: usize, weight: f32| {
            let mut v = vec![0.0; DIMS];
            v[axis] = 1.0;
            v[1] = weight;
            super::super::prepare(&v).unwrap()
        };
        let query = blend(0, 1.0);
        let bank: Vec<Vec<f32>> = (0..40).map(|i| blend(400 + i, 0.8 + 0.01 * i as f32)).collect();
        let mut store = Store::new("m");
        for i in 0..30 {
            let part = Part::new(PartKind::Image, 0, String::new(), &at(0.0, 100 + i));
            store.insert(PathBuf::from(format!("/bg/{i:02}.jpg")), doc(vec![part]));
        }
        let sticky = Part::new(PartKind::Image, 0, String::new(), &blend(300, 3.0));
        store.insert("/shots/sticky.png".into(), doc(vec![sticky]));
        let cat = Part::new(PartKind::Image, 0, String::new(), &blend(0, 0.2));
        store.insert("/photos/cat.jpg".into(), doc(vec![cat]));
        let sticky_score =
            store.get(Path::new("/shots/sticky.png")).unwrap().parts[0].score(&query);
        let cat_score = store.get(Path::new("/photos/cat.jpg")).unwrap().parts[0].score(&query);
        assert!(sticky_score > 0.6, "{sticky_score}");
        // Без мерки «липкая» картинка выделяется среди фона так же, как настоящая.
        let before = store.search(&query, &[], MIN_RANK, 10, |_| true);
        assert_eq!(before.len(), 2, "{before:?}");
        store.set_bank(bank.clone());
        assert!(store.has_bank());
        let hits = store.search(&query, &[], MIN_RANK, 10, |_| true);
        let paths: Vec<_> = hits.iter().map(|h| h.path.to_string_lossy().into_owned()).collect();
        assert_eq!(paths, ["/photos/cat.jpg"]);
        assert!((hits[0].score - cat_score).abs() < 1e-6);
        // Добавленное после мерки меряется сразу.
        let late = Part::new(PartKind::Text, 0, String::new(), &blend(300, 3.0));
        store.insert("/docs/late.txt".into(), doc(vec![late]));
        let hits = store.search(&query, &[], MIN_RANK, 10, |_| true);
        assert_eq!(hits.len(), 1, "{hits:?}");
    }

    #[test]
    fn search_in_parallel_matches_single_thread() {
        let mut store = Store::new("m");
        for i in 0..5000 {
            let cosine = 0.2 + 0.0001 * (i % 997) as f32;
            let part = Part::new(PartKind::Text, 0, String::new(), &at(cosine, 1 + i % 500));
            store.insert(PathBuf::from(format!("/f/{i:05}.txt")), doc(vec![part]));
        }
        store.insert(
            "/f/best.txt".into(),
            doc(vec![Part::new(PartKind::Text, 0, String::new(), &at(0.9, 501))]),
        );
        let hits = store.search(&query(), &[], MIN_RANK, 50, |_| true);
        assert_eq!(hits[0].path, Path::new("/f/best.txt"));
        assert!(hits.windows(2).all(|w| w[0].rank >= w[1].rank));
        assert!(hits.len() < 50, "фон отсечён: {}", hits.len());
    }

    #[test]
    fn freshness_and_removal() {
        let mut store = sample();
        assert!(store.fresh(Path::new("/docs/lease.docx"), 10, 100));
        assert!(!store.fresh(Path::new("/docs/lease.docx"), 11, 100));
        assert!(!store.fresh(Path::new("/nope"), 10, 100));
        assert!(store.remove(Path::new("/docs/lease.docx")));
        assert!(!store.remove(Path::new("/docs/lease.docx")));
        assert_eq!(store.retain(|p| p.starts_with("/photos")), 181);
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
        // Векторы имён из старых индексов при чтении отбрасываются.
        assert!(loaded.get(Path::new("/bg/name01.lnk")).unwrap().parts.is_empty());
        assert_eq!(loaded.get(Path::new("/photos/cat.jpg")).unwrap().parts.len(), 1);
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
