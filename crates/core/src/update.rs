//! Проверка обновлений: какой выпуск на GitHub Releases новее этой версии.
//!
//! Запрос делает воркер (по кнопке, без фоновой службы), здесь — разбор ответа
//! `GET /repos/<владелец>/<репозиторий>/releases` и сравнение версий.

use std::cmp::Ordering;

use serde::Deserialize;

/// Репозиторий выпусков на GitHub.
pub const REPOSITORY: &str = "midnightheartsgames/MH-Files";

/// Путь запроса списка выпусков (новые первыми).
pub fn releases_path() -> String {
    format!("/repos/{REPOSITORY}/releases?per_page=20")
}

/// Версия `1.2.3` или `1.2.3-rc.1` (SemVer без метаданных сборки).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Части пред-выпуска (`rc.1` → `["rc", "1"]`); пусто — обычный выпуск.
    pub pre: Vec<String>,
}

impl Version {
    /// Разбор `1.2.3`, `v1.2.3-rc.1`, `1.2.3+build` (метаданные сборки отбрасываются).
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut numbers = core.split('.').map(|part| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse::<u64>().ok())
                .flatten()
        });
        let major = numbers.next()??;
        let minor = numbers.next()??;
        let patch = numbers.next()??;
        if numbers.next().is_some() {
            return None;
        }
        let pre = match pre {
            Some(pre) => {
                let parts: Vec<String> = pre.split('.').map(str::to_string).collect();
                if parts.iter().any(|part| part.is_empty()) {
                    return None;
                }
                parts
            }
            None => Vec::new(),
        };
        Some(Version { major, minor, patch, pre })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                // Выпуск старше любого своего пред-выпуска.
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => compare_pre(&self.pre, &other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// Правила SemVer: числа сравниваются как числа и младше слов; короче — младше.
fn compare_pre(a: &[String], b: &[String]) -> Ordering {
    for (a, b) in a.iter().zip(b) {
        let order = match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => a.cmp(b),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

/// Выпуск, который стоит предложить.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// Страница выпуска.
    pub url: String,
    pub name: String,
    /// Дата публикации `ГГГГ-ММ-ДД`, если есть.
    pub published: Option<String>,
}

/// Итог проверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Новее этой версии ничего нет.
    UpToDate,
    Available(Release),
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
}

/// Самый новый выпуск из ответа GitHub новее `current`. Пред-выпуски предлагаются, только
/// если и сама программа — пред-выпуск (rc ставят те, кто хочет проверять новое); черновики
/// и теги не по SemVer пропускаются.
pub fn check(json: &str, current: &str) -> Result<Check, String> {
    let current = Version::parse(current).ok_or_else(|| format!("непонятная версия {current}"))?;
    let releases: Vec<RawRelease> =
        serde_json::from_str(json).map_err(|error| format!("непонятный ответ GitHub: {error}"))?;
    let newest = releases
        .into_iter()
        .filter(|raw| !raw.draft)
        .filter_map(|raw| {
            let version = Version::parse(&raw.tag_name)?;
            let pre = raw.prerelease || version.is_prerelease();
            (current.is_prerelease() || !pre).then_some((version, raw))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b));
    Ok(match newest {
        Some((version, raw)) if version > current => Check::Available(Release {
            name: raw
                .name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("MH Files {version}")),
            published: raw.published_at.and_then(|at| at.get(..10).map(str::to_string)),
            version,
            url: raw.html_url,
        }),
        _ => Check::UpToDate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn parses_and_orders_versions() {
        assert_eq!(v("v1.2.3").to_string(), "1.2.3");
        assert_eq!(v("1.0.0-rc.1+abc").to_string(), "1.0.0-rc.1");
        for bad in ["", "1.2", "1.2.3.4", "1.x.3", "1.2.3-", "1.2.3-rc..1", "-1.2.3"] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
        let ordered = [
            "0.5.0",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-beta",
            "1.0.0-rc.1",
            "1.0.0-rc.2",
            "1.0.0-rc.10",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "10.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("1.0.0").cmp(&v("v1.0.0")), Ordering::Equal);
    }

    fn releases(list: &[(&str, bool, bool)]) -> String {
        let items: Vec<String> = list
            .iter()
            .map(|(tag, prerelease, draft)| {
                format!(
                    r#"{{"tag_name":"{tag}","name":"","html_url":"https://example.org/{tag}","draft":{draft},"prerelease":{prerelease},"published_at":"2026-10-01T10:00:00Z"}}"#
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn picks_newest_suitable_release() {
        let json = releases(&[
            ("v1.0.0-rc.1", true, false),
            ("v1.0.0", false, false),
            ("v1.1.0-rc.1", true, false),
            ("v2.0.0", false, true),
            ("nightly", false, false),
        ]);
        // Пред-выпуск видит и новые пред-выпуски.
        let Check::Available(release) = check(&json, "1.0.0-rc.1").unwrap() else { panic!() };
        assert_eq!(release.version, v("1.1.0-rc.1"));
        assert_eq!(release.url, "https://example.org/v1.1.0-rc.1");
        assert_eq!(release.name, "MH Files 1.1.0-rc.1");
        assert_eq!(release.published.as_deref(), Some("2026-10-01"));
        // Обычный выпуск — только обычные; черновик 2.0.0 не в счёт.
        assert_eq!(check(&json, "1.0.0").unwrap(), Check::UpToDate);
        let Check::Available(release) = check(&json, "0.5.0").unwrap() else { panic!() };
        assert_eq!(release.version, v("1.0.0"));
        assert_eq!(check("[]", "1.0.0").unwrap(), Check::UpToDate);
        assert!(check("{\"message\":\"rate limit\"}", "1.0.0").is_err());
        assert!(check("[]", "непонятно").is_err());
    }
}
