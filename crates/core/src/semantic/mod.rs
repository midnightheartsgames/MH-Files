//! Поиск по смыслу: модель EmbeddingGemma 2 переводит запрос, текст файлов и картинки в
//! векторы одного пространства — близкие по смыслу вещи получают близкие векторы. «Кошка на
//! диване» находит фотографию кошки, `IMG_0412.jpg`, а «договор аренды» — скан договора.
//!
//! Здесь — то, что не трогает диск и модель: как оформить текст для модели (у запросов и у
//! документов разные подсказки), как нарезать длинный текст, как достать текст из документов
//! Office, как хранить векторы и искать по ним ([`store`]), какие файлы скачать
//! ([`model`]). Модель работает в отдельном процессе `llama-server` (llama.cpp) — им
//! управляет `mh-files-fs`.

pub mod model;
pub mod store;

/// Длина хранимого вектора. Модель отдаёт 768 чисел, но обучена так (Matryoshka), что
/// начало вектора — тоже вектор: 512 чисел почти не теряют в качестве, а индекс на треть
/// меньше и быстрее.
pub const DIMS: usize = 512;

/// Версия подсказок и нарезки. Меняется — индекс строится заново: векторы с другими
/// подсказками с новыми несравнимы.
pub const PROMPT_PROFILE: &str = "mh-eg2-v1";

/// Сколько символов текста в одном куске. Модель понимает до 8 тысяч токенов, но на коротких
/// кусках точнее: смысл абзаца не размывается соседними.
pub const CHUNK_CHARS: usize = 1200;
/// Сколько кусков текста брать из файла: начало документа почти всегда говорит, о чём он.
pub const MAX_CHUNKS: usize = 6;
/// Сколько страниц PDF смотреть.
pub const PDF_PAGES: u32 = 2;
/// Картинка для модели — не больше этой стороны: крупнее модель всё равно уменьшает, а
/// уменьшенная заранее даёт одинаковый вектор при любом исходном размере.
pub const IMAGE_SIDE: u32 = 768;
/// Длина отрывка, который показывается рядом с найденным.
pub const SNIPPET_CHARS: usize = 140;

/// Запрос пользователя — с подсказкой задачи, как модель учили.
pub fn query_prompt(query: &str) -> String {
    format!("task: search result | query: {}", query.trim())
}

/// Кусок документа: заголовок (имя файла) и текст.
pub fn document_prompt(title: &str, text: &str) -> String {
    let title = title.trim();
    let title = if title.is_empty() { "none" } else { title };
    format!("title: {title} | text: {}", text.trim())
}

/// Разделители и «верблюжий» регистр — в пробелы.
fn split_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut previous: Option<char> = None;
    for c in text.chars() {
        let separator =
            matches!(c, '_' | '-' | '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | ',');
        if separator || c.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            // invoiceMarch → invoice March; цифры от букв тоже отделяются: report2024.
            let boundary = previous.is_some_and(|p| {
                (p.is_lowercase() && c.is_uppercase())
                    || (p.is_alphabetic() && c.is_ascii_digit())
                    || (p.is_ascii_digit() && c.is_alphabetic())
            });
            if boundary && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push(c);
        }
        previous = Some(c);
    }
    out.trim().to_string()
}

/// Служебные слова: в имени файла они ничего не значат («кошка на диване» — ищутся «кошка»
/// и «диване»).
const STOPWORDS: &[&str] = &[
    "в", "во", "на", "и", "с", "со", "по", "к", "ко", "о", "об", "у", "за", "из", "от", "до",
    "для", "про", "под", "над", "или", "а", "но", "не", "мой", "моя", "мои", "the", "a", "an",
    "of", "in", "on", "at", "to", "for", "with", "and", "or", "my",
];

/// Слова запроса для сравнения с именами: в нижнем регистре, «ё» как «е», без служебных.
pub fn query_words(query: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in plain(query).split(|c: char| !c.is_alphanumeric()) {
        if word.chars().count() >= 2
            && !STOPWORDS.contains(&word)
            && !words.iter().any(|w| w == word)
        {
            words.push(word.to_string());
        }
    }
    words
}

/// Слова имени файла и его папки — так же, как [`query_words`], но со словами из
/// «верблюжьего» регистра и цифр: `invoiceMarch2024` → invoice, march, 2024.
pub fn name_tokens(path: &std::path::Path) -> Vec<String> {
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let mut text = split_words(crate::entry::stem_of(&name, false));
    if let Some(parent) = path.parent().and_then(|p| p.file_name()) {
        text.push(' ');
        text.push_str(&split_words(&parent.to_string_lossy()));
    }
    plain(&text)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// Доля слов запроса, которые есть среди слов имени (0…1). Слово из четырёх букв и длиннее
/// совпадает и с другими окончаниями: «кошка» — с «кошки», «кошкой», но не с «кошмар».
pub fn name_coverage(tokens: &[String], words: &[String]) -> f32 {
    if words.is_empty() {
        return 0.0;
    }
    let found = words
        .iter()
        .filter(|word| {
            let stem = stem(word);
            tokens.iter().any(|t| t == *word || stem.is_some_and(|s| t.starts_with(s)))
        })
        .count();
    found as f32 / words.len() as f32
}

/// Основа слова без конечных гласных (`кошка` → `кошк`, `диване` → `диван`); у коротких
/// слов основы нет — они совпадают только целиком («кот» — не «котлеты»).
fn stem(word: &str) -> Option<&str> {
    if word.chars().count() < 4 {
        return None;
    }
    let mut stem = word;
    while stem.chars().count() > 3
        && let Some(last) = stem.chars().last()
        && "аеиоуыэюяйьaeiouy".contains(last)
    {
        stem = &stem[..stem.len() - last.len_utf8()];
    }
    Some(stem)
}

fn plain(text: &str) -> String {
    text.to_lowercase().replace('ё', "е")
}

/// Пробелы и переводы строк — по одному: модели они ничего не говорят, а место занимают.
pub fn squeeze(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(&line);
    }
    out
}

/// Нарезать текст на куски не длиннее `max_chars` символов, не больше `max_chunks`. Режется
/// по абзацам, длинный абзац — по предложениям, а сплошной текст — по словам.
pub fn chunks(text: &str, max_chars: usize, max_chunks: usize) -> Vec<String> {
    let text = squeeze(text);
    let max_chars = max_chars.max(16);
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;
    let push = |piece: &str, out: &mut Vec<String>, current: &mut String, len: &mut usize| {
        let piece_len = piece.chars().count();
        if *len > 0 && *len + 1 + piece_len > max_chars {
            out.push(std::mem::take(current));
            *len = 0;
        }
        if *len > 0 {
            current.push(' ');
            *len += 1;
        }
        current.push_str(piece);
        *len += piece_len;
    };
    for paragraph in text.split("\n\n") {
        if out.len() >= max_chunks {
            break;
        }
        let paragraph = paragraph.replace('\n', " ");
        for piece in pieces(&paragraph, max_chars) {
            push(&piece, &mut out, &mut current, &mut current_len);
            if out.len() >= max_chunks {
                break;
            }
        }
    }
    if !current.trim().is_empty() && out.len() < max_chunks {
        out.push(current);
    }
    out.retain(|chunk| chunk.chars().any(char::is_alphanumeric));
    out
}

/// Абзац кусками не длиннее `max_chars`: предложения, а слишком длинное предложение — слова.
fn pieces(paragraph: &str, max_chars: usize) -> Vec<String> {
    if paragraph.chars().count() <= max_chars {
        return vec![paragraph.to_string()];
    }
    let mut out = Vec::new();
    let mut sentence = String::new();
    let flush = |sentence: &mut String, out: &mut Vec<String>| {
        let text = sentence.trim();
        if text.is_empty() {
            sentence.clear();
            return;
        }
        if text.chars().count() <= max_chars {
            out.push(text.to_string());
        } else {
            let mut part = String::new();
            for word in text.split_whitespace() {
                // Слово длиннее куска (base64, хэш) — режется по символам.
                let word: String = word.chars().take(max_chars).collect();
                if !part.is_empty() && part.chars().count() + 1 + word.chars().count() > max_chars {
                    out.push(std::mem::take(&mut part));
                }
                if !part.is_empty() {
                    part.push(' ');
                }
                part.push_str(&word);
            }
            if !part.is_empty() {
                out.push(part);
            }
        }
        sentence.clear();
    };
    for c in paragraph.chars() {
        sentence.push(c);
        if matches!(c, '.' | '!' | '?' | '…' | '\n') {
            flush(&mut sentence, &mut out);
        }
    }
    flush(&mut sentence, &mut out);
    out
}

/// Начало текста для подписи найденного: одна строка, не длиннее [`SNIPPET_CHARS`].
pub fn snippet(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= SNIPPET_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(SNIPPET_CHARS - 1).collect();
    if let Some(space) = cut.rfind(' ').filter(|&at| at > SNIPPET_CHARS / 2) {
        cut.truncate(space);
    }
    cut.push('…');
    cut
}

/// Какие части архива Office/OpenDocument содержат текст, в порядке чтения: у docx —
/// документ, у pptx — слайды по номерам, у xlsx — строки ячеек, у OpenDocument — `content.xml`.
pub fn office_parts(ext: &str, names: &[String]) -> Vec<String> {
    match ext {
        "docx" | "docm" | "dotx" => {
            names.iter().filter(|n| *n == "word/document.xml").cloned().collect()
        }
        "pptx" | "pptm" | "ppsx" => {
            let mut slides: Vec<(u32, String)> = names
                .iter()
                .filter_map(|n| {
                    let number = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?;
                    Some((number.parse().ok()?, n.clone()))
                })
                .collect();
            slides.sort();
            slides.into_iter().map(|(_, n)| n).collect()
        }
        "xlsx" | "xlsm" => names.iter().filter(|n| *n == "xl/sharedStrings.xml").cloned().collect(),
        "odt" | "ods" | "odp" => names.iter().filter(|n| *n == "content.xml").cloned().collect(),
        _ => Vec::new(),
    }
}

/// Документ Office, из которого берётся текст.
pub fn is_office(ext: &str) -> bool {
    matches!(
        ext,
        "docx"
            | "docm"
            | "dotx"
            | "pptx"
            | "pptm"
            | "ppsx"
            | "xlsx"
            | "xlsm"
            | "odt"
            | "ods"
            | "odp"
    )
}

/// Текст из XML Office: теги убираются, конец абзаца, строки таблицы, слайда — перевод
/// строки, сущности раскрываются.
pub fn xml_text(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() / 3);
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        push_unescaped(&mut out, &rest[..open]);
        let Some(close) = rest[open..].find('>') else { break };
        let tag = &rest[open + 1..open + close];
        let name = tag.trim_start_matches('/').split([' ', '/']).next().unwrap_or("");
        let local = name.rsplit(':').next().unwrap_or(name);
        let closing = tag.starts_with('/');
        match local {
            // Абзац, строка таблицы, ячейка Excel, абзац OpenDocument, разрыв строки.
            "p" | "tr" | "si" | "h" if closing => out.push('\n'),
            "br" | "cr" => out.push('\n'),
            "tab" | "tc" | "s" if !closing => out.push(' '),
            "tc" | "table-cell" if closing => out.push(' '),
            _ => {}
        }
        rest = &rest[open + close + 1..];
    }
    push_unescaped(&mut out, rest);
    squeeze(&out)
}

fn push_unescaped(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|&s| s <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
}

/// Вектор от модели — к хранимому виду: первые [`DIMS`] чисел, длина 1. `None` — вектор
/// негодный (короче, с NaN, нулевой).
pub fn prepare(vector: &[f32]) -> Option<Vec<f32>> {
    if vector.len() < DIMS {
        return None;
    }
    let mut out = vector[..DIMS].to_vec();
    let norm = out.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || norm < 1e-6 {
        return None;
    }
    for v in &mut out {
        *v /= norm;
    }
    Some(out)
}

/// Сколько секунд звука давать модели: её кодировщик слушает окнами по 30 секунд.
pub const AUDIO_SECONDS: u32 = 30;
/// Сколько байт MP3 брать: 30 секунд при 128 кбит/с; при большем битрейте — меньше секунд,
/// но для смысла записи и этого хватает.
pub const MP3_BYTES: usize = 480 * 1024;

/// Размер тега ID3v2 в начале MP3 (вместе с заголовком): обложка альбома бывает в мегабайты,
/// модели она не нужна. 0 — тега нет.
pub fn id3_size(head: &[u8]) -> usize {
    if head.len() < 10 || &head[..3] != b"ID3" {
        return 0;
    }
    // Размер — четыре байта по 7 бит («syncsafe»); флаг 0x10 — есть ещё нижний колонтитул.
    let size = head[6..10].iter().fold(0usize, |acc, &b| (acc << 7) | usize::from(b & 0x7F));
    let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
    10 + size + footer
}

/// Начало WAV не длиннее `seconds`: заголовок пересобирается под новую длину. `None` — не
/// WAV или без нужных частей.
pub fn wav_head(bytes: &[u8], seconds: u32) -> Option<Vec<u8>> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut fmt: Option<&[u8]> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = at + 8;
        if id == b"fmt " {
            fmt = Some(bytes.get(body..body + len)?);
        } else if id == b"data" {
            let fmt = fmt?;
            if fmt.len() < 16 {
                return None;
            }
            let byte_rate = u32::from_le_bytes(fmt[8..12].try_into().ok()?) as usize;
            let align = usize::from(u16::from_le_bytes(fmt[12..14].try_into().ok()?)).max(1);
            let available = len.min(bytes.len().saturating_sub(body));
            let mut take = available.min(byte_rate.saturating_mul(seconds as usize));
            take -= take % align;
            let mut out = Vec::with_capacity(44 + fmt.len() + take);
            out.extend_from_slice(b"RIFF");
            out.extend_from_slice(&((4 + 8 + fmt.len() + 8 + take) as u32).to_le_bytes());
            out.extend_from_slice(b"WAVE");
            out.extend_from_slice(b"fmt ");
            out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
            out.extend_from_slice(fmt);
            out.extend_from_slice(b"data");
            out.extend_from_slice(&(take as u32).to_le_bytes());
            out.extend_from_slice(&bytes[body..body + take]);
            return Some(out);
        }
        // Части выровнены по двум байтам.
        at = body + len + (len & 1);
    }
    None
}

/// Base64 (RFC 4648, с `=`) — картинки уходят модели внутри JSON как `data:` URI.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn prompts_follow_the_model_card() {
        assert_eq!(
            query_prompt("  кошка на диване "),
            "task: search result | query: кошка на диване"
        );
        assert_eq!(document_prompt("Отчёт", "Итоги года"), "title: Отчёт | text: Итоги года");
        assert_eq!(document_prompt(" ", "текст"), "title: none | text: текст");
    }

    #[test]
    fn names_become_words() {
        let tokens = |p: &str| name_tokens(Path::new(p)).join(" ");
        assert_eq!(
            tokens("/фото/Море_2024/IMG_2024-07 Отпуск.final.jpg"),
            "img 2024 07 отпуск final море 2024"
        );
        assert_eq!(tokens("invoiceMarch2024.pdf"), "invoice march 2024");
        assert_eq!(tokens("/x/Ёлка (2).png"), "елка 2 x");
    }

    #[test]
    fn query_words_match_names_with_other_endings() {
        let words = query_words("Кошка на диване, ёлка и кот");
        assert_eq!(words, ["кошка", "диване", "елка", "кот"]);
        let cover = |name: &str, query: &str| {
            name_coverage(&name_tokens(Path::new(name)), &query_words(query))
        };
        assert_eq!(cover("/фото/кошки/IMG_1.jpg", "кошка"), 1.0);
        assert_eq!(cover("Кошкой_на_диванчике.jpg", "кошка на диване"), 1.0);
        assert_eq!(cover("Ночной кошмар.png", "кошка"), 0.0);
        assert_eq!(cover("Котлеты.lnk", "кот"), 0.0, "короткое слово — только целиком");
        assert_eq!(cover("кот.jpg", "кот"), 1.0);
        assert_eq!(cover("cat_videos.zip", "кошка cat"), 0.5);
        assert_eq!(cover("x.txt", ""), 0.0);
    }

    #[test]
    fn chunks_respect_paragraphs_and_limits() {
        let text = "Первый абзац.\n\n\nВторой   абзац\nс переносом.\n\nТретий.";
        assert_eq!(chunks(text, 1000, 6), ["Первый абзац. Второй абзац с переносом. Третий."]);
        let parts = chunks(text, 20, 6);
        assert!(parts.iter().all(|c| c.chars().count() <= 20), "{parts:?}");
        assert_eq!(parts[0], "Первый абзац.");
        // Сплошной текст без точек режется по словам; лишние куски не берутся.
        let long = "слово ".repeat(500);
        let parts = chunks(&long, 100, 3);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|c| c.chars().count() <= 100));
        // Одно огромное «слово» тоже режется.
        let blob = "x".repeat(1000);
        assert!(chunks(&blob, 100, 6).iter().all(|c| c.chars().count() <= 100));
        assert!(chunks("  \n\n ---- \n", 100, 6).is_empty(), "без букв и цифр — нечего искать");
    }

    #[test]
    fn snippet_is_one_short_line() {
        assert_eq!(snippet("коротко\nи ясно"), "коротко и ясно");
        let long = "слово ".repeat(100);
        let cut = snippet(&long);
        assert!(cut.ends_with('…') && cut.chars().count() <= SNIPPET_CHARS);
    }

    #[test]
    fn office_parts_in_reading_order() {
        let names: Vec<String> = [
            "ppt/slides/slide10.xml",
            "ppt/slides/slide2.xml",
            "ppt/slides/_rels/slide2.xml.rels",
            "ppt/slides/slide1.xml",
            "word/document.xml",
            "content.xml",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            office_parts("pptx", &names),
            ["ppt/slides/slide1.xml", "ppt/slides/slide2.xml", "ppt/slides/slide10.xml"]
        );
        assert_eq!(office_parts("docx", &names), ["word/document.xml"]);
        assert_eq!(office_parts("odt", &names), ["content.xml"]);
        assert!(office_parts("pdf", &names).is_empty());
        assert!(is_office("xlsx") && is_office("odp") && !is_office("pdf") && !is_office("doc"));
    }

    #[test]
    fn xml_text_keeps_paragraphs() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:t>Договор</w:t></w:r><w:r><w:tab/><w:t xml:space="preserve">аренды &amp; найма</w:t></w:r></w:p><w:p><w:r><w:t>Цена: 5&#160;000 &#x20BD;</w:t></w:r></w:p></w:body></w:document>"#;
        assert_eq!(xml_text(xml), "Договор аренды & найма\nЦена: 5 000 ₽");
        assert_eq!(xml_text("<a>AT&T &bogus; x</a>"), "AT&T &bogus; x");
        let ods = "<text:p>Яблоки</text:p><table:table-cell/><text:p>Груши</text:p>";
        assert_eq!(xml_text(ods), "Яблоки\nГруши");
    }

    #[test]
    fn vectors_are_cut_and_normalized() {
        let mut raw = vec![0.0f32; 768];
        raw[0] = 3.0;
        raw[1] = 4.0;
        raw[700] = 100.0; // за пределами DIMS — отбрасывается.
        let v = prepare(&raw).unwrap();
        assert_eq!(v.len(), DIMS);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert!(prepare(&[1.0; 10]).is_none(), "короткий");
        assert!(prepare(&vec![0.0; 768]).is_none(), "нулевой");
        let mut nan = vec![1.0f32; 768];
        nan[3] = f32::NAN;
        assert!(prepare(&nan).is_none());
    }

    #[test]
    fn id3_tags_are_skipped() {
        assert_eq!(id3_size(b"ID3\x04\x00\x00\x00\x00\x02\x01rest"), 10 + 257);
        assert_eq!(id3_size(b"ID3\x04\x00\x10\x00\x00\x00\x05"), 10 + 5 + 10);
        assert_eq!(id3_size(b"\xFF\xFBframe....."), 0);
        assert_eq!(id3_size(b"ID3"), 0);
    }

    #[test]
    fn wav_is_trimmed_with_a_valid_header() {
        // 8 кГц, моно, 16 бит: 16 000 байт в секунду; 3 секунды и лишняя часть после данных.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        wav.extend_from_slice(b"LIST\x03\0\0\0abc\0"); // нечётная длина — с выравниванием
        wav.extend_from_slice(b"fmt \x10\0\0\0");
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend(std::iter::repeat_n(7u8, 48000));
        let head = wav_head(&wav, 2).unwrap();
        assert_eq!(head.len(), 44 + 32000);
        assert_eq!(&head[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize, head.len() - 8);
        assert_eq!(u32::from_le_bytes(head[40..44].try_into().unwrap()), 32000);
        let all = wav_head(&wav, 30).unwrap();
        assert_eq!(all.len(), 44 + 48000, "короче предела — целиком");
        // Обрезанный файл: данных меньше, чем обещано.
        let cut = wav_head(&wav[..wav.len() - 1001], 30).unwrap();
        assert_eq!(cut.len(), 44 + 46998, "по границе отсчёта");
        assert!(wav_head(b"RIFF\0\0\0\0AVI ", 30).is_none());
        assert!(
            wav_head(&wav[..60], 30).is_none() || wav_head(&wav[..60], 30).unwrap().len() <= 60
        );
    }

    #[test]
    fn base64_matches_rfc() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xFF, 0xFE, 0x00]), "//4A");
    }
}
