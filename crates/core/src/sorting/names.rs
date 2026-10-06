//! Имена и пути для сортировщика: сравнение путей, расширения, свободные имена, имена папок,
//! допустимые в Windows. Перенесено из MH Sort без изменений поведения.

use std::path::{MAIN_SEPARATOR, Path, PathBuf};

/// Ключ для сравнения путей. На Windows регистр и вид разделителя не важны.
pub fn path_key(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    let raw = raw.replace('/', "\\").to_lowercase();
    #[cfg(not(windows))]
    let raw = raw.into_owned();
    let trimmed = raw.trim_end_matches(MAIN_SEPARATOR);
    if trimmed.is_empty() { raw } else { trimmed.to_owned() }
}

/// `child` совпадает с `parent` или лежит внутри него.
pub fn is_within(child: &Path, parent: &Path) -> bool {
    let child = path_key(child);
    let parent = path_key(parent);
    if child == parent {
        return true;
    }
    if parent.ends_with(MAIN_SEPARATOR) {
        return child.starts_with(&parent);
    }
    child.starts_with(&parent) && child[parent.len()..].starts_with(MAIN_SEPARATOR)
}

/// Основное расширение файла (после последней точки), в нижнем регистре.
/// Пустая строка, если расширения нет или «хвост» на него не похож
/// (например, «Mr. Smith» или «файл.»).
pub fn primary_ext(file_name: &str) -> String {
    let body = file_name.trim_start_matches('.');
    match body.rfind('.') {
        Some(i) if looks_like_ext(&body[i + 1..]) => body[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

pub fn looks_like_ext(ext: &str) -> bool {
    !ext.is_empty()
        && ext.chars().count() <= 16
        && !ext.contains('.')
        && !ext.chars().any(char::is_whitespace)
        && ext.chars().any(char::is_alphanumeric)
}

/// Делит имя на основу и расширение с точкой: ("archive", ".tar.gz").
/// `ext` — уже известное расширение без точки; пустое значит «расширения нет».
pub fn split_name<'a>(name: &'a str, ext: &str) -> (&'a str, &'a str) {
    if ext.is_empty() {
        return (name, "");
    }
    if name.len() > ext.len() + 1 {
        let cut = name.len() - ext.len() - 1;
        if name.is_char_boundary(cut)
            && name[cut..].starts_with('.')
            && name[cut + 1..].to_lowercase() == ext
        {
            return (&name[..cut], &name[cut..]);
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Первое свободное имя в папке: «image.jpg», «image (1).jpg», «image (2).jpg»…
pub fn unique_path(dir: &Path, name: &str, ext: &str, is_taken: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !is_taken(&first) {
        return first;
    }
    let (stem, suffix) = split_name(name, ext);
    (1u32..)
        .map(|n| dir.join(format!("{stem} ({n}){suffix}")))
        .find(|p| !is_taken(p))
        .expect("свободное имя всегда находится")
}

/// Делает строку допустимым именем папки Windows.
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    // Windows не разрешает пробел или точку в конце имени
    let mut result = cleaned.trim().trim_end_matches(['.', ' ']).to_owned();
    if result.is_empty() {
        result.push('_');
    }
    // Имена устройств (CON, NUL, COM1…) запрещены даже с расширением
    let stem_len = result.find('.').unwrap_or(result.len());
    let stem = result[..stem_len].to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        result.insert(stem_len, '_');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_splitting() {
        assert_eq!(split_name("photo.jpg", "jpg"), ("photo", ".jpg"));
        assert_eq!(split_name("backup.TAR.GZ", "tar.gz"), ("backup", ".TAR.GZ"));
        assert_eq!(split_name("Mr. Smith", ""), ("Mr. Smith", ""));
        assert_eq!(primary_ext("Фото.JPEG"), "jpeg");
        assert_eq!(primary_ext(".gitignore"), "");
        assert_eq!(primary_ext("Mr. Smith goes"), "");
        assert_eq!(primary_ext("файл."), "");
    }

    #[test]
    fn unique_names() {
        let taken = ["/x/a.jpg", "/x/a (1).jpg"].map(PathBuf::from);
        let path =
            unique_path(Path::new("/x"), "a.jpg", "jpg", |p| taken.contains(&p.to_path_buf()));
        assert_eq!(path, PathBuf::from("/x").join("a (2).jpg"));
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize_name("Игры / ресурсы"), "Игры _ ресурсы");
        assert_eq!(sanitize_name("CON"), "CON_");
        assert_eq!(sanitize_name("com1.txt"), "com1_.txt");
        assert_eq!(sanitize_name("COM0"), "COM0");
        assert_eq!(sanitize_name("Видео. "), "Видео");
        assert_eq!(sanitize_name(""), "_");
    }

    #[cfg(windows)]
    #[test]
    fn paths_compare_case_insensitive() {
        assert!(is_within(Path::new(r"D:\Downloads\Видео\a.mp4"), Path::new(r"d:/downloads/")));
        assert!(!is_within(Path::new(r"D:\Downloads2"), Path::new(r"D:\Downloads")));
        assert!(is_within(Path::new(r"D:\x"), Path::new(r"D:\")));
        assert_eq!(path_key(Path::new(r"D:\Видео\")), path_key(Path::new("d:/видео")));
    }
}
