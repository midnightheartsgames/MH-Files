//! Применение плана пакетного переименования: два прохода и откат при сбое.

use std::path::PathBuf;

use mh_files_core::rename::Plan;

/// Переименовывает всё или ничего. Возвращает число переименованных объектов.
pub fn apply(plan: &Plan) -> Result<usize, String> {
    let token = format!("{:x}", std::process::id() ^ nanos());
    let (first, second) = plan.passes(&token);
    let mut done_first: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (from, to) in &first {
        if let Err(error) = std::fs::rename(from, to) {
            rollback(&done_first);
            return Err(format!("{}: {error}", from.display()));
        }
        done_first.push((from.clone(), to.clone()));
    }
    let mut done_second: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (from, to) in &second {
        // Чужой файл мог появиться после проверки: не затирать его.
        if to.exists() && !same_file_ignoring_case(from, to) {
            rollback(&done_second);
            rollback(&done_first);
            return Err(format!("уже существует: {}", to.display()));
        }
        if let Err(error) = std::fs::rename(from, to) {
            rollback(&done_second);
            rollback(&done_first);
            return Err(format!("{}: {error}", to.display()));
        }
        done_second.push((from.clone(), to.clone()));
    }
    Ok(plan.moves.len())
}

fn same_file_ignoring_case(a: &std::path::Path, b: &std::path::Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Возвращает сделанное в обратном порядке. Ошибки отката уже ничего не изменят — пропускаются.
fn rollback(done: &[(PathBuf, PathBuf)]) {
    for (from, to) in done.iter().rev() {
        let _ = std::fs::rename(to, from);
    }
}

fn nanos() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_names_and_rolls_back_on_failure() {
        let dir = std::env::temp_dir().join(format!("mh-files-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), "A").unwrap();
        std::fs::write(dir.join("b"), "B").unwrap();
        let plan =
            Plan { moves: vec![(dir.join("a"), dir.join("b")), (dir.join("b"), dir.join("a"))] };
        assert_eq!(apply(&plan), Ok(2));
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "B");

        let broken = Plan {
            moves: vec![(dir.join("a"), dir.join("c")), (dir.join("missing"), dir.join("d"))],
        };
        assert!(apply(&broken).is_err());
        assert!(dir.join("a").exists(), "откат вернул первый файл");
        assert!(!dir.join("c").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
