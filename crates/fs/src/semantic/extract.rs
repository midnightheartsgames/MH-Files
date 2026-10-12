//! Что из файла отдать модели: имя словами — всегда, а по настройкам ещё куски текста,
//! картинку, кадр видео, страницы PDF, начало звукозаписи.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use mh_files_core::category::Category;
use mh_files_core::entry::extension_of;
use mh_files_core::semantic::model::Input;
use mh_files_core::semantic::store::PartKind;
use mh_files_core::semantic::{self as model, CHUNK_CHARS, IMAGE_SIDE, MAX_CHUNKS, PDF_PAGES};
use mh_files_core::settings::SemanticSettings;
use mh_files_platform::thumbs::{Bitmap, ImageMode};

/// Текста читается не больше: [`MAX_CHUNKS`] кусков всё равно берутся из начала.
const TEXT_BYTES: u64 = 64 * 1024;
/// Части документа Office читаются не больше: огромная таблица — не повод ждать.
const OFFICE_BYTES: u64 = 8 << 20;

/// Часть файла для модели.
#[derive(Debug, Clone)]
pub struct Item {
    pub kind: PartKind,
    pub number: u16,
    pub snippet: String,
    pub input: Input,
}

impl Item {
    /// Картинка или звук: такие считаются заметно дольше текста.
    pub fn is_media(&self) -> bool {
        !matches!(self.input, Input::Text(_))
    }
}

fn text_item(kind: PartKind, number: u16, snippet: String, text: String) -> Item {
    Item { kind, number, snippet, input: Input::Text(text) }
}

/// Части файла `path` (`size` байт). Ошибки чтения содержимого не страшны: имя есть всегда.
pub fn items(path: &Path, size: u64, settings: &SemanticSettings) -> Vec<Item> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = extension_of(&name);
    let mut items = vec![text_item(
        PartKind::Name,
        0,
        String::new(),
        model::document_prompt("none", &model::name_words(path)),
    )];
    if size > u64::from(settings.max_file_mb) << 20 {
        return items;
    }
    let title = mh_files_core::entry::stem_of(&name, false).to_string();
    let texts = |text: &str, items: &mut Vec<Item>| {
        for (number, chunk) in model::chunks(text, CHUNK_CHARS, MAX_CHUNKS).into_iter().enumerate()
        {
            let snippet = model::snippet(&chunk);
            let prompt = model::document_prompt(&title, &chunk);
            items.push(text_item(PartKind::Text, number as u16, snippet, prompt));
        }
    };
    if settings.documents {
        if crate::preview::text_like(&ext) && ext != "svg"
            || (ext.is_empty() || ext == "nfo") && crate::preview::sniff_text(path)
        {
            if let Ok(text) = read_text(path) {
                texts(&text, &mut items);
            }
        } else if model::is_office(&ext) {
            if let Ok(text) = office_text(path, &ext) {
                texts(&text, &mut items);
            }
        } else if ext == "pdf" {
            items.extend(pdf_pages(path));
        }
    }
    if settings.images {
        let picture = if Category::Images.extensions().contains(&ext.as_str()) {
            image(path, &ext)
        } else if Category::Video.extensions().contains(&ext.as_str()) {
            // Кадр видео — эскиз Windows: там, где для типа есть обработчик эскизов.
            shell_picture(path)
        } else {
            None
        };
        if let Some(input) = picture {
            items.push(Item { kind: PartKind::Image, number: 0, snippet: String::new(), input });
        }
    }
    if settings.audio
        && let Some(base64) = audio(path, &ext)
    {
        items.push(Item {
            kind: PartKind::Audio,
            number: 0,
            snippet: String::new(),
            input: Input::Audio { base64 },
        });
    }
    items
}

fn read_text(path: &Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(TEXT_BYTES).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(crate::preview::decode_text(&bytes).0)
}

/// Текст документа Office или OpenDocument: это zip с XML внутри.
fn office_text(path: &Path, ext: &str) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    let names: Vec<String> = archive.file_names().map(String::from).collect();
    let mut text = String::new();
    for part in model::office_parts(ext, &names) {
        let entry = archive.by_name(&part).map_err(|e| e.to_string())?;
        let mut xml = String::new();
        entry.take(OFFICE_BYTES).read_to_string(&mut xml).map_err(|e| e.to_string())?;
        text.push_str(&model::xml_text(&xml));
        text.push_str("\n\n");
        // Слайдов и листов бывает много — начала хватает.
        if text.chars().count() > CHUNK_CHARS * MAX_CHUNKS {
            break;
        }
    }
    Ok(text)
}

/// Первые страницы PDF картинками (движок PDF Windows).
fn pdf_pages(path: &Path) -> Vec<Item> {
    let mut items = Vec::new();
    let mut pages = PDF_PAGES;
    let mut page = 0;
    while page < pages {
        let Ok((png, count)) = mh_files_platform::pdf::render_page(path, page, IMAGE_SIDE) else {
            break;
        };
        pages = pages.min(count);
        items.push(Item {
            kind: PartKind::Page,
            number: page as u16,
            snippet: String::new(),
            input: Input::Image { mime: "image/png", base64: model::base64(&png) },
        });
        page += 1;
    }
    items
}

/// Картинка, уменьшенная до [`IMAGE_SIDE`], в JPEG.
fn image(path: &Path, ext: &str) -> Option<Input> {
    let bitmap = if crate::images::vector(ext) {
        crate::images::render_svg_file(path, IMAGE_SIDE, 4 << 20).ok().map(|(bitmap, _)| bitmap)
    } else if crate::images::decodable(ext) {
        crate::images::decode_scaled(path, IMAGE_SIDE, u64::MAX).ok()
    } else {
        None
    };
    // HEIC, RAW, PSD — через эскизы Windows, если кодеки стоят.
    match bitmap {
        Some(bitmap) => jpeg(&bitmap),
        None => shell_picture(path),
    }
}

fn shell_picture(path: &Path) -> Option<Input> {
    let bitmap =
        mh_files_platform::thumbs::shell_image(path, IMAGE_SIDE, ImageMode::Thumbnail).ok()?;
    jpeg(&bitmap)
}

/// RGBA → JPEG (прозрачное — на белом): webp и прочее llama.cpp сам не читает, а JPEG
/// в несколько раз меньше PNG.
pub fn jpeg(bitmap: &Bitmap) -> Option<Input> {
    if bitmap.width == 0 || bitmap.height == 0 {
        return None;
    }
    let rgb: Vec<u8> = bitmap
        .rgba
        .chunks_exact(4)
        .flat_map(|p| {
            let alpha = u16::from(p[3]);
            let blend = |c: u8| ((u16::from(c) * alpha + 255 * (255 - alpha)) / 255) as u8;
            [blend(p[0]), blend(p[1]), blend(p[2])]
        })
        .collect();
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88);
    image::ImageEncoder::write_image(
        encoder,
        &rgb,
        bitmap.width,
        bitmap.height,
        image::ExtendedColorType::Rgb8,
    )
    .ok()?;
    Some(Input::Image { mime: "image/jpeg", base64: model::base64(&out) })
}

/// Начало звукозаписи в base64: MP3 — без тега с обложкой, WAV — первые 30 секунд.
fn audio(path: &Path, ext: &str) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    match ext {
        "mp3" => {
            let mut head = [0u8; 10];
            let read = file.read(&mut head).ok()?;
            let skip = model::id3_size(&head[..read]);
            file.seek(SeekFrom::Start(skip as u64)).ok()?;
            let mut bytes = Vec::new();
            file.take(model::MP3_BYTES as u64).read_to_end(&mut bytes).ok()?;
            (bytes.len() > 1024).then(|| model::base64(&bytes))
        }
        "wav" => {
            // Заголовок и 30 секунд даже 32-битного стерео 192 кГц — меньше 50 МБ.
            let mut bytes = Vec::new();
            file.take(48 << 20).read_to_end(&mut bytes).ok()?;
            let head = model::wav_head(&bytes, model::AUDIO_SECONDS)?;
            (head.len() > 1024).then(|| model::base64(&head))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> SemanticSettings {
        SemanticSettings::default()
    }

    #[test]
    fn text_files_give_name_and_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("заметки_отпуск.md");
        std::fs::write(&path, "# Поездка на море\n\nВзять купальник и крем от солнца.").unwrap();
        let items = items(&path, 100, &settings());
        assert_eq!(items[0].kind, PartKind::Name);
        let Input::Text(name) = &items[0].input else { panic!() };
        assert!(name.starts_with("title: none | text: заметки отпуск"), "{name}");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].kind, PartKind::Text);
        let Input::Text(chunk) = &items[1].input else { panic!() };
        assert!(chunk.starts_with("title: заметки_отпуск | text: # Поездка на море"), "{chunk}");
        assert!(items[1].snippet.contains("купальник"));
        // Без «документов» — только имя; слишком большой файл — тоже.
        let off = SemanticSettings { documents: false, ..settings() };
        assert_eq!(super::items(&path, 100, &off).len(), 1);
        assert_eq!(super::items(&path, 65 << 20, &settings()).len(), 1);
    }

    #[test]
    fn office_documents_give_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("договор.docx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("word/document.xml", options).unwrap();
        std::io::Write::write_all(
            &mut zip,
            "<w:document><w:p><w:r><w:t>Договор аренды квартиры</w:t></w:r></w:p></w:document>"
                .as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap();
        let items = items(&path, 500, &settings());
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[1].snippet, "Договор аренды квартиры");
    }

    #[test]
    fn pictures_become_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("кот.png");
        let mut picture = image::RgbaImage::new(1600, 900);
        for (x, _, pixel) in picture.enumerate_pixels_mut() {
            *pixel = image::Rgba([(x % 256) as u8, 80, 160, if x < 800 { 255 } else { 0 }]);
        }
        picture.save(&path).unwrap();
        let items = items(&path, 1000, &settings());
        let image = items.iter().find(|i| i.kind == PartKind::Image).unwrap();
        assert!(image.is_media());
        let Input::Image { mime, base64 } = &image.input else { panic!() };
        assert_eq!(*mime, "image/jpeg");
        assert!(base64.starts_with("/9j/"), "JPEG начинается с FF D8 FF");
        let no_images = SemanticSettings { images: false, ..settings() };
        assert!(super::items(&path, 1000, &no_images).iter().all(|i| !i.is_media()));
    }

    #[test]
    fn audio_heads() {
        let dir = tempfile::tempdir().unwrap();
        let mp3 = dir.path().join("песня.mp3");
        let mut bytes = b"ID3\x04\x00\x00\x00\x00\x00\x10".to_vec();
        bytes.extend(std::iter::repeat_n(0xAA, 16)); // тег
        bytes.extend(std::iter::repeat_n(0xFF, 600 * 1024)); // «кадры»
        std::fs::write(&mp3, &bytes).unwrap();
        let items = items(&mp3, bytes.len() as u64, &settings());
        let audio = items.iter().find(|i| i.kind == PartKind::Audio).unwrap();
        let Input::Audio { base64 } = &audio.input else { panic!() };
        assert_eq!(base64.len(), mh_files_core::semantic::MP3_BYTES.div_ceil(3) * 4);
        assert!(base64.starts_with("////"), "тег пропущен");
        let quiet = SemanticSettings { audio: false, ..settings() };
        assert!(super::items(&mp3, 10, &quiet).iter().all(|i| i.kind != PartKind::Audio));
    }
}
