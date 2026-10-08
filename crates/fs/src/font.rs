//! Просмотр шрифтов: образцы текста на языках раскладок клавиатуры, набранные самим шрифтом.
//! Картинка собирается как SVG и рисуется resvg; подмены шрифта нет — буквы, которых в
//! шрифте нет, не рисуются, а язык без своих букв уходит в сведения «Нет букв».

use std::sync::Arc;

use mh_files_core::font_samples::{DIGITS, samples};
use resvg::usvg::{self, fontdb};

use crate::preview::Preview;

/// Файлы шрифтов, которые умеет разобрать fontdb (sfnt: TrueType, OpenType, коллекции).
pub fn is_font(ext: &str) -> bool {
    matches!(ext, "ttf" | "otf" | "ttc" | "otc")
}

/// Больше шрифт не читается: крупные шрифты с иероглифами — десятки мегабайт.
pub const FONT_LIMIT: u64 = 64 << 20;

const BACKGROUND: &str = "#12161D";
const TEXT: &str = "#E6E9EF";
const DIM: &str = "#A9A9AD";

/// Образец шрифта картинкой. `max_side` — 512 в Инспекторе, больше — в быстром просмотре:
/// раскладка там крупнее. `languages` — языки раскладок (BCP 47).
pub fn preview(data: Vec<u8>, max_side: u32, languages: &[String]) -> Result<Preview, String> {
    let mut db = fontdb::Database::new();
    db.load_font_data(data);
    let faces: Vec<fontdb::FaceInfo> = db.faces().cloned().collect();
    let face = faces.first().ok_or("файл шрифта не читается")?;
    let id = face.id;
    let family = face.families.first().map(|(name, _)| name.clone()).unwrap_or_default();
    // Есть ли в шрифте все буквы текста (пробелы не в счёт).
    let covers = |text: &str| {
        db.with_face_data(id, |data, index| {
            use skrifa::MetadataProvider;
            let font = skrifa::FontRef::from_index(data, index).ok()?;
            let charmap = font.charmap();
            Some(text.chars().filter(|c| !c.is_whitespace()).all(|c| charmap.map(c).is_some()))
        })
        .flatten()
        .unwrap_or(false)
    };
    let mut shown = Vec::new();
    let mut missing = Vec::new();
    for (name, text) in samples(languages) {
        if covers(text) { shown.push((name, text)) } else { missing.push(name) }
    }
    let digits: String = DIGITS.chars().filter(|c| covers(&c.to_string())).collect();

    let quick = max_side > 600;
    let width = if quick { 1000.0 } else { 340.0 };
    let (big, large, small) = if quick { (64.0, 28.0, 17.0) } else { (40.0, 22.0, 16.0) };
    let mut lines: Vec<(f32, &str, String)> = Vec::new();
    let specimen: String = ["Aa", "Бб", "Gg", "Жж", "Qq", "Яя", "0123"]
        .into_iter()
        .filter(|s| covers(s))
        .collect::<Vec<_>>()
        .join(" ");
    if !specimen.is_empty() {
        lines.push((big, TEXT, specimen));
    }
    for (_, text) in &shown {
        for line in wrap(text, width, large) {
            lines.push((large, TEXT, line));
        }
        for line in wrap(text, width, small) {
            lines.push((small, DIM, line));
        }
    }
    if !digits.trim().is_empty() {
        for line in wrap(&digits, width, large) {
            lines.push((large, DIM, line));
        }
    }
    if lines.is_empty() {
        return Err("в шрифте нет букв для образца".into());
    }

    let pad = 18.0;
    let mut y = pad;
    let mut body = String::new();
    for (size, color, text) in &lines {
        y += size * 1.25;
        body.push_str(&format!(
            r#"<text x="{pad}" y="{y:.1}" font-size="{size}" fill="{color}" xml:space="preserve">{}</text>"#,
            escape(text)
        ));
        y += size * 0.35;
    }
    let height = y + pad;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height:.0}"><rect width="100%" height="100%" fill="{BACKGROUND}"/>{body}</svg>"#
    );
    let mut options = usvg::Options { fontdb: Arc::new(db), ..Default::default() };
    // Всегда этот шрифт, и никаких подмен: показываем только то, что в нём есть.
    options.font_resolver = usvg::FontResolver {
        select_font: Box::new(move |_, _| Some(id)),
        select_fallback: Box::new(|_, _, _| None),
    };
    let tree = usvg::Tree::from_str(&svg, &options).map_err(|e| format!("шрифт: {e}"))?;
    // Чётче на мониторах с масштабом: рисуется крупнее, окно уменьшит.
    let scale = (max_side as f32 / width).clamp(1.0, 2.0);
    let bitmap = crate::images::rasterize(
        &tree,
        (width * scale).round() as u32,
        (height * scale).round() as u32,
    )?;

    let mut info = vec![("Шрифт".to_string(), family)];
    info.push(("Начертание".into(), style(face)));
    if faces.len() > 1 {
        info.push(("Начертаний в файле".into(), faces.len().to_string()));
    }
    if !shown.is_empty() {
        info.push(("Языки".into(), shown.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")));
    }
    if !missing.is_empty() {
        info.push(("Нет букв".into(), missing.join(", ")));
    }
    Ok(Preview::Image { bitmap, dimensions: None, pages: None, info })
}

/// «Обычный», «Полужирный курсив» и т. п.
fn style(face: &fontdb::FaceInfo) -> String {
    let weight = match face.weight.0 {
        0..=150 => "Тонкий",
        151..=250 => "Сверхсветлый",
        251..=350 => "Светлый",
        351..=450 => "Обычный",
        451..=550 => "Средний",
        551..=650 => "Полужирный",
        651..=750 => "Жирный",
        751..=850 => "Сверхжирный",
        _ => "Чёрный",
    };
    match face.style {
        fontdb::Style::Normal => weight.to_string(),
        fontdb::Style::Italic | fontdb::Style::Oblique => format!("{weight} курсив"),
    }
}

/// Перенос по словам, чтобы строка шрифта `size` уместилась в `width` (ширина буквы —
/// около 0,55 кегля; у иероглифов — кегль).
fn wrap(text: &str, width: f32, size: f32) -> Vec<String> {
    let char_width = |c: char| if (c as u32) >= 0x2E80 { size } else { size * 0.55 };
    let limit = width - 36.0;
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0.0;
    let words: Vec<&str> = if text.contains(' ') {
        text.split(' ').collect()
    } else {
        // Текст без пробелов (китайский, японский) — переносится где угодно.
        text.char_indices().map(|(i, c)| &text[i..i + c.len_utf8()]).collect()
    };
    let spaced = text.contains(' ');
    for word in words {
        let w: f32 = word.chars().map(char_width).sum();
        let space = if line.is_empty() || !spaced { 0.0 } else { size * 0.3 };
        if !line.is_empty() && used + space + w > limit {
            lines.push(std::mem::take(&mut line));
            used = 0.0;
        } else if spaced && !line.is_empty() {
            line.push(' ');
            used += space;
        }
        line.push_str(word);
        used += w;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_long_lines_by_words() {
        let lines = wrap("The quick brown fox jumps over the lazy dog", 200.0, 20.0);
        assert!(lines.len() > 1, "{lines:?}");
        assert_eq!(lines.join(" "), "The quick brown fox jumps over the lazy dog");
        assert_eq!(wrap("коротко", 1000.0, 20.0), ["коротко"]);
        let cjk = wrap("天地玄黄宇宙洪荒日月盈昃", 120.0, 20.0);
        assert!(cjk.len() > 1 && cjk.concat() == "天地玄黄宇宙洪荒日月盈昃");
    }

    #[test]
    fn escapes_markup() {
        assert_eq!(escape("a<b & \"c\">"), "a&lt;b &amp; &quot;c&quot;&gt;");
    }

    #[test]
    fn renders_a_system_font_if_there_is_one() {
        // Шрифтов в репозитории нет; берём любой системный, если он найдётся.
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let Some(path) = db.faces().find_map(|face| match &face.source {
            fontdb::Source::File(path)
                if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttf")) =>
            {
                Some(path.clone())
            }
            _ => None,
        }) else {
            return;
        };
        let data = std::fs::read(&path).unwrap();
        let languages = ["en-US".to_string(), "ru-RU".to_string()];
        match preview(data, 2048, &languages).unwrap() {
            Preview::Image { bitmap, info, .. } => {
                assert!(bitmap.width >= 1000 && bitmap.height > 50);
                assert_eq!(info[0].0, "Шрифт");
                assert!(info.iter().any(|(k, _)| k == "Языки" || k == "Нет букв"));
            }
            other => panic!("{other:?}"),
        }
        assert!(preview(b"not a font".to_vec(), 512, &languages).is_err());
    }
}
