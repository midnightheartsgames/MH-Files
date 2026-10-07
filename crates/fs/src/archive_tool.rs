//! Архивы, которые MH Files сам не читает (rar), — через внешнюю программу: 7-Zip (`7z.exe`),
//! а без него `tar.exe` Windows (в Windows 11 он читает rar). Своего кода RAR нет: его
//! распаковщик — не свободная программа, а внешний процесс лицензию не затрагивает.
//!
//! Оглавление разбирается из вывода программы, записи читаются по одной из её stdout.

use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::OnceLock;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    SevenZip(PathBuf),
    Tar(PathBuf),
}

/// Запись оглавления в том виде, как её видит программа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawItem {
    /// Имя для обращения к записи (как его напечатала программа).
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// Чем читать — ищется один раз.
pub fn tool() -> Option<&'static Tool> {
    static TOOL: OnceLock<Option<Tool>> = OnceLock::new();
    TOOL.get_or_init(find).as_ref()
}

pub const MISSING: &str = "для rar нужен 7-Zip (7-zip.org) или Windows 11 — его tar.exe читает rar";

fn find() -> Option<Tool> {
    if cfg!(windows) {
        let program_files = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(|dir| PathBuf::from(dir).join("7-Zip").join("7z.exe"));
        if let Some(seven) = program_files.into_iter().find(|path| path.is_file()) {
            return Some(Tool::SevenZip(seven));
        }
        let tar = std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32").join("tar.exe"))
            .filter(|path| path.is_file());
        return tar.map(Tool::Tar);
    }
    let in_path = |name: &str| {
        std::env::var_os("PATH")?
            .to_string_lossy()
            .split(':')
            .map(|dir| Path::new(dir).join(name))
            .find(|path| path.is_file())
    };
    ["7zz", "7z"]
        .into_iter()
        .find_map(in_path)
        .map(Tool::SevenZip)
        .or_else(|| in_path("bsdtar").map(Tool::Tar))
}

fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    // bsdtar печатает и принимает имена в кодировке локали: пусть это будет UTF-8.
    #[cfg(not(windows))]
    command.env("LC_ALL", "C.UTF-8");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Без мелькающего окна консоли.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// Оглавление архива.
pub fn list(tool: &Tool, archive: &Path) -> Result<Vec<RawItem>, String> {
    let output = match tool {
        Tool::SevenZip(program) => command(program)
            .args(["l", "-slt", "-ba", "-sccUTF-8", "-p"])
            .arg("--")
            .arg(archive)
            .output(),
        Tool::Tar(program) => command(program).arg("-tvf").arg(archive).output(),
    }
    .map_err(|error| format!("архиватор не запущен: {error}"))?;
    let text = decode_output(tool, &output.stdout);
    if !output.status.success() {
        return Err(failure(&output.stderr, &text));
    }
    Ok(match tool {
        Tool::SevenZip(_) => parse_seven(&text),
        Tool::Tar(_) => parse_tar(&text),
    })
}

/// Поток содержимого одной записи. Процесс завершается, когда поток прочитан или брошен.
pub struct Entry {
    child: Child,
    stdout: ChildStdout,
}

impl std::io::Read for Entry {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Entry {
    /// Дочитано: программа сказала, что всё в порядке?
    pub fn finish(mut self) -> Result<(), String> {
        match self.child.wait() {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => Err("архиватор не смог прочитать запись (повреждён или зашифрован?)".into()),
            Err(error) => Err(error.to_string()),
        }
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        // Брошенный поток (отмена, предел размера) — процесс больше не нужен.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Открыть запись `name` на чтение.
pub fn open(tool: &Tool, archive: &Path, name: &str) -> Result<Entry, String> {
    let mut command = match tool {
        // -spd: имя как есть, без масок * и ?.
        Tool::SevenZip(program) => {
            let mut command = command(program);
            command.args(["x", "-so", "-spd", "-sccUTF-8", "-p"]).arg("--").arg(archive).arg(name);
            command
        }
        Tool::Tar(program) => {
            let mut command = command(program);
            command.arg("-xOf").arg(archive).arg("--").arg(name);
            command
        }
    };
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("архиватор не запущен: {error}"))?;
    let stdout = child.stdout.take().ok_or("нет вывода архиватора")?;
    Ok(Entry { child, stdout })
}

/// Вывод 7-Zip — UTF-8 (`-sccUTF-8`). `tar.exe` Windows печатает имена в кодировке ANSI
/// (у русской Windows — 1251).
fn decode_output(tool: &Tool, bytes: &[u8]) -> String {
    if matches!(tool, Tool::Tar(_))
        && cfg!(windows)
        && std::str::from_utf8(bytes).is_err()
        && let Some(encoding) = ansi_encoding(mh_files_platform::shell::ansi_code_page())
    {
        return encoding.decode_without_bom_handling(bytes).0.into_owned();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Кодировка Windows по номеру кодовой страницы ANSI.
fn ansi_encoding(code_page: u32) -> Option<&'static encoding_rs::Encoding> {
    use encoding_rs::*;
    Some(match code_page {
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1252 => WINDOWS_1252,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        65001 => UTF_8,
        _ => return None,
    })
}

fn failure(stderr: &[u8], stdout: &str) -> String {
    let text = String::from_utf8_lossy(stderr);
    let all = format!("{text}\n{stdout}").to_lowercase();
    if all.contains("password") || all.contains("encrypted") || all.contains("парол") {
        return "архив зашифрован — откройте его в архиваторе".into();
    }
    let line = text.lines().chain(stdout.lines()).map(str::trim).find(|l| !l.is_empty());
    match line {
        Some(line) => format!("архив не прочитан: {line}"),
        None => "архив повреждён или не поддерживается".into(),
    }
}

/// `7z l -slt -ba`: блоки «Ключ = значение», разделённые пустой строкой.
fn parse_seven(text: &str) -> Vec<RawItem> {
    let mut items = Vec::new();
    let mut current: Option<RawItem> = None;
    for line in text.lines().chain(std::iter::once("")) {
        let line = line.trim_end_matches('\r');
        let Some((key, value)) = line.split_once(" = ") else {
            if line.trim().is_empty() {
                items.extend(current.take());
            }
            continue;
        };
        match key {
            "Path" => {
                items.extend(current.take());
                current = Some(RawItem {
                    name: value.to_string(),
                    is_dir: false,
                    size: 0,
                    modified: None,
                });
            }
            "Folder" => {
                if let Some(item) = &mut current {
                    item.is_dir = value == "+";
                }
            }
            "Attributes" => {
                if let Some(item) = &mut current {
                    item.is_dir |= value.starts_with('D');
                }
            }
            "Size" => {
                if let Some(item) = &mut current {
                    item.size = value.parse().unwrap_or(0);
                }
            }
            "Modified" => {
                if let Some(item) = &mut current {
                    item.modified = local_time(value.get(..19).unwrap_or(value));
                }
            }
            _ => {}
        }
    }
    items.retain(|item| !item.name.is_empty());
    items
}

/// `tar -tvf`: как `ls -l` — права, ссылки, владелец, группа, размер, дата из трёх частей и
/// имя до конца строки.
fn parse_tar(text: &str) -> Vec<RawItem> {
    let mut items = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let mut rest = line;
        let mut fields = Vec::with_capacity(8);
        while fields.len() < 8 {
            rest = rest.trim_start();
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            if end == 0 {
                break;
            }
            fields.push(&rest[..end]);
            rest = &rest[end..];
        }
        // Ровно один пробел отделяет имя: пробелы в начале имени сохраняются.
        let name = rest.strip_prefix(' ').unwrap_or(rest);
        if fields.len() < 8 || name.is_empty() {
            continue;
        }
        // Символическая ссылка: «имя -> цель».
        let name = if fields[0].starts_with('l') {
            name.split(" -> ").next().unwrap_or(name)
        } else {
            name
        };
        items.push(RawItem {
            name: name.to_string(),
            is_dir: fields[0].starts_with('d') || name.ends_with('/'),
            size: fields[4].parse().unwrap_or(0),
            modified: None,
        });
    }
    items
}

fn local_time(text: &str) -> Option<SystemTime> {
    use chrono::TimeZone;
    let time = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").ok()?;
    Some(chrono::Local.from_local_datetime(&time).earliest()?.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seven_zip_listing() {
        let text = "Path = docs\r\nFolder = +\r\nSize = 0\r\nModified = 2026-10-07 12:00:00\r\n\r\n\
                    Path = docs\\read me.txt\r\nFolder = -\r\nSize = 5\r\nModified = 2026-10-07 12:00:01.1234567\r\nAttributes = A\r\n\r\n\
                    Path = empty\r\nSize = 0\r\nAttributes = D\r\n";
        let items = parse_seven(text);
        assert_eq!(items.len(), 3);
        assert!(items[0].is_dir && items[2].is_dir);
        assert_eq!(items[1].name, "docs\\read me.txt");
        assert_eq!(items[1].size, 5);
        assert!(items[1].modified.is_some());
    }

    #[test]
    fn parses_tar_listing() {
        let text = "drwxr-xr-x  0 0      0           0 Oct  7 12:00 docs/\n\
                    -rw-r--r--  0 user   group       5 Oct  7  2025 docs/read me.txt\n\
                    lrwxr-xr-x  0 0      0           0 Oct  7 12:00 link -> docs\n\
                    junk\n";
        let items = parse_tar(text);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["docs/", "docs/read me.txt", "link"]);
        assert!(items[0].is_dir && !items[1].is_dir);
        assert_eq!(items[1].size, 5);
    }

    /// С настоящей программой (если она есть): zip читается так же, как rar.
    #[test]
    fn reads_through_installed_tools() {
        let mut tools: Vec<Tool> = tool().into_iter().cloned().collect();
        if let Some(bsdtar) = ["/usr/bin/bsdtar", "/usr/local/bin/bsdtar"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
        {
            tools.push(Tool::Tar(bsdtar));
        }
        for tool in &tools {
            read_through(tool);
        }
    }

    fn read_through(tool: &Tool) {
        let dir = std::env::temp_dir().join(format!(
            "mh-files-tool-{}-{}",
            std::process::id(),
            matches!(tool, Tool::Tar(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("t.zip");
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("docs/привет мир.txt", options).unwrap();
            writer.write_all(b"hello").unwrap();
            writer.finish().unwrap();
        }
        let items = list(tool, &archive).unwrap();
        let item = items.iter().find(|i| i.name.ends_with("привет мир.txt")).unwrap();
        assert_eq!(item.size, 5);
        let mut entry = open(tool, &archive, &item.name).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
        entry.finish().unwrap();
        assert_eq!(bytes, b"hello");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
