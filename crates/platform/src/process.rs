//! Фоновый процесс-помощник (`llama-server` поиска по смыслу): без окна консоли, с низким
//! приоритетом, и в Windows — в задании (job), которое убивает его вместе с MH Files, даже
//! если программа упала: брошенный процесс держал бы гигабайт памяти.

use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Запущенный процесс. Уничтожение объекта завершает процесс.
pub struct Background {
    child: Child,
    #[cfg(windows)]
    _job: Option<crate::win::process::Job>,
}

impl Background {
    /// Запустить `program` с `args`; вывод (stdout и stderr) — в файл `log`, если он задан.
    pub fn spawn(
        program: &Path,
        args: &[String],
        log: Option<&Path>,
    ) -> Result<Background, String> {
        let mut command = Command::new(program);
        command.args(args).stdin(Stdio::null());
        if let Some(dir) = program.parent().filter(|d| !d.as_os_str().is_empty()) {
            command.current_dir(dir);
        }
        match log.map(std::fs::File::create) {
            Some(Ok(file)) => {
                let copy = file.try_clone().map_err(|e| e.to_string())?;
                command.stdout(Stdio::from(copy)).stderr(Stdio::from(file));
            }
            _ => {
                command.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(crate::win::process::CREATION_FLAGS);
        }
        let child = command
            .spawn()
            .map_err(|error| format!("{} не запущен: {error}", program.display()))?;
        #[cfg(windows)]
        {
            let job = crate::win::process::Job::adopt(&child);
            Ok(Background { child, _job: job })
        }
        #[cfg(not(windows))]
        Ok(Background { child })
    }

    /// Процесс уже завершился: с каким кодом.
    pub fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(match status.code() {
                Some(code) => format!("код {code}"),
                None => "прерван".to_string(),
            }),
            Ok(None) => None,
            Err(error) => Some(error.to_string()),
        }
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn runs_logs_and_stops() {
        let dir = std::env::temp_dir().join(format!("mh-process-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log.txt");
        let mut quick = Background::spawn(
            Path::new("/bin/sh"),
            &["-c".into(), "echo привет; echo ошибка >&2".into()],
            Some(&log),
        )
        .unwrap();
        let started = std::time::Instant::now();
        while quick.exited().is_none() && started.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(quick.exited().as_deref(), Some("код 0"));
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("привет") && text.contains("ошибка"), "{text}");
        let mut slow = Background::spawn(Path::new("/bin/sleep"), &["30".into()], None).unwrap();
        assert!(slow.exited().is_none());
        let pid = slow.id();
        drop(slow);
        assert!(!Path::new(&format!("/proc/{pid}")).exists(), "завершён и прибран");
        assert!(Background::spawn(&dir.join("нет такой программы"), &[], None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
