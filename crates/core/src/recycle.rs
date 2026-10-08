//! Корзина Windows изнутри: у каждого удалённого объекта в `X:\$Recycle.Bin\<SID>` два
//! файла — `$Rxxxxxx.ext` (сам объект) и `$Ixxxxxx.ext` (сведения: откуда удалён, когда,
//! сколько весил). Здесь — разбор сведений, без ввода-вывода.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Сведения об удалённом объекте из `$I`-файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deleted {
    /// Где объект лежал до удаления.
    pub original: PathBuf,
    /// Байты (у папки — всё содержимое).
    pub size: u64,
    pub deleted: Option<SystemTime>,
}

/// Секунд между 1601-01-01 (начало FILETIME) и 1970-01-01.
const FILETIME_TO_UNIX: u64 = 11_644_473_600;
/// В Windows Vista–8.1 путь — всегда 260 знаков UTF-16.
const V1_PATH_CHARS: usize = 260;

/// Разобрать `$I`-файл: версия 1 (Vista–8.1, путь фиксированной длины) или 2 (Windows 10 и
/// новее, длина пути перед ним). `None` — не сведения корзины.
pub fn parse_info(data: &[u8]) -> Option<Deleted> {
    let u64_at = |at: usize| Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?));
    let version = u64_at(0)?;
    let size = u64_at(8)?;
    let filetime = u64_at(16)?;
    let (start, chars): (usize, usize) = match version {
        1 => (24, V1_PATH_CHARS),
        2 => {
            let chars = u32::from_le_bytes(data.get(24..28)?.try_into().ok()?) as usize;
            (28, chars)
        }
        _ => return None,
    };
    let bytes = data.get(start..start.checked_add(chars.checked_mul(2)?)?)?;
    let wide: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    let original = String::from_utf16(&wide).ok()?;
    if original.is_empty() {
        return None;
    }
    let deleted = (filetime / 10_000_000).checked_sub(FILETIME_TO_UNIX).map(|seconds| {
        UNIX_EPOCH
            + Duration::from_secs(seconds)
            + Duration::from_nanos(filetime % 10_000_000 * 100)
    });
    Some(Deleted { original: PathBuf::from(original), size, deleted })
}

/// Имя файла с самим объектом для имени сведений: `$IAB12CD.txt` → `$RAB12CD.txt`.
pub fn data_name(info_name: &str) -> Option<String> {
    info_name.strip_prefix("$I").map(|rest| format!("$R{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(text: &str) -> Vec<u8> {
        text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
    }

    fn header(version: u64, size: u64, filetime: u64) -> Vec<u8> {
        [version, size, filetime].iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// 2026-10-08 12:00:00 UTC в FILETIME.
    const NOON: u64 = (1_791_460_800 + FILETIME_TO_UNIX) * 10_000_000;

    #[test]
    fn parses_windows_10_info() {
        let path = r"C:\Users\Аня\Документы\отчёт.docx";
        let mut data = header(2, 12_345, NOON);
        data.extend((path.encode_utf16().count() as u32 + 1).to_le_bytes());
        data.extend(wide(path));
        let deleted = parse_info(&data).unwrap();
        assert_eq!(deleted.original, PathBuf::from(path));
        assert_eq!(deleted.size, 12_345);
        assert_eq!(deleted.deleted, Some(UNIX_EPOCH + Duration::from_secs(1_791_460_800)));
    }

    #[test]
    fn parses_windows_8_info() {
        let mut data = header(1, 7, NOON);
        let mut path = wide(r"D:\old.txt");
        path.resize(V1_PATH_CHARS * 2, 0);
        data.extend(path);
        let deleted = parse_info(&data).unwrap();
        assert_eq!(deleted.original, PathBuf::from(r"D:\old.txt"));
        assert_eq!(deleted.size, 7);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_info(b"short"), None);
        assert_eq!(parse_info(&header(3, 1, NOON)), None, "неизвестная версия");
        let mut truncated = header(2, 1, NOON);
        truncated.extend(100u32.to_le_bytes());
        truncated.extend(wide("C:"));
        assert_eq!(parse_info(&truncated), None, "путь короче заявленного");
        assert_eq!(data_name("$IAB12CD.txt").as_deref(), Some("$RAB12CD.txt"));
        assert_eq!(data_name("desktop.ini"), None);
    }
}
