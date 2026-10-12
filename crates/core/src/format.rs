//! Подписи для людей: размеры, даты, количество.

use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local};

/// `824 МБ`, `4 КБ`, `0 байт`. Двоичные единицы, как в Проводнике.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["КБ", "МБ", "ГБ", "ТБ", "ПБ"];
    if bytes < 1024 {
        return format!("{bytes} байт");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{:.1} {}", value, UNITS[unit]).replace('.', ",")
    } else {
        format!("{:.0} {}", value, UNITS[unit])
    }
}

/// Размер в столбце списка: файлы меньше килобайта всё равно `1 КБ`, как в Проводнике.
pub fn size_column(bytes: u64) -> String {
    if bytes == 0 {
        return "0 КБ".into();
    }
    if bytes < 1024 { "1 КБ".into() } else { size(bytes) }
}

/// `Сегодня 01:32`, `Вчера 14:02`, `19.09.2026 21:42`.
pub fn date(time: SystemTime) -> String {
    date_relative_to(time, Local::now())
}

pub fn date_relative_to(time: SystemTime, now: DateTime<Local>) -> String {
    let time: DateTime<Local> = time.into();
    let today = now.date_naive();
    let day = time.date_naive();
    if day == today {
        format!("Сегодня {}", time.format("%H:%M"))
    } else if today.pred_opt() == Some(day) {
        format!("Вчера {}", time.format("%H:%M"))
    } else if day.year() == today.year() {
        time.format("%d.%m.%Y %H:%M").to_string()
    } else {
        time.format("%d.%m.%Y").to_string()
    }
}

/// Полная дата для Инспектора.
pub fn date_full(time: SystemTime) -> String {
    let time: DateTime<Local> = time.into();
    time.format("%d.%m.%Y %H:%M:%S").to_string()
}

/// `1 объект`, `3 объекта`, `128 объектов`.
pub fn items(count: usize) -> String {
    format!("{} {}", self::count(count), plural(count, "объект", "объекта", "объектов"))
}

/// Число с разрядами через пробел: `1 234 567`.
pub fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// Русское множественное число.
pub fn plural<'a>(n: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let (n10, n100) = (n % 10, n % 100);
    if n10 == 1 && n100 != 11 {
        one
    } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
        few
    } else {
        many
    }
}

/// Тип файла по расширению: `PNG`, `Папка`, `Файл`.
pub fn kind(ext: &str, is_dir: bool) -> String {
    if is_dir {
        "Папка".into()
    } else if ext.is_empty() {
        "Файл".into()
    } else {
        ext.to_uppercase()
    }
}

/// Давность даты от 0 (только что) до 1 (год и старше) — для цвета кружка у даты. Шкала по
/// привычным отметкам: последний час — чисто красный, к суткам — оранжевый, к неделе —
/// жёлтый, к месяцу — жёлто-зелёный, к году — зелёный. Между отметками —
/// плавно по логарифму. Дата из будущего — как «только что».
pub fn age(time: SystemTime, now: SystemTime) -> f32 {
    const HOUR: f64 = 3600.0;
    const DAY: f64 = 24.0 * HOUR;
    /// Отметки: давность в секундах и доля шкалы. Доли подобраны под цвета по умолчанию
    /// (красный → бирюзово-зелёный): сутки — оранжевый, неделя — жёлтый.
    const MARKS: [(f64, f64); 5] =
        [(HOUR, 0.0), (DAY, 0.2), (7.0 * DAY, 0.4), (30.0 * DAY, 0.65), (365.0 * DAY, 1.0)];
    let seconds = now.duration_since(time).map_or(0.0, |d| d.as_secs_f64());
    if seconds <= MARKS[0].0 {
        return 0.0;
    }
    for pair in MARKS.windows(2) {
        let ((from, a), (to, b)) = (pair[0], pair[1]);
        if seconds <= to {
            let k = (seconds / from).ln() / (to / from).ln();
            return (a + (b - a) * k) as f32;
        }
    }
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn ages_grow_from_new_to_old() {
        use std::time::Duration;
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let ago = |secs: u64| age(now - Duration::from_secs(secs), now);
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert_eq!(ago(10), 0.0);
        assert_eq!(ago(50 * 60), 0.0, "последний час — красный");
        assert_eq!(age(now + Duration::from_secs(60), now), 0.0, "из будущего — новое");
        assert!(close(ago(86_400), 0.2), "сутки — оранжевый");
        assert!(close(ago(7 * 86_400), 0.4), "неделя — жёлтый");
        assert!(close(ago(30 * 86_400), 0.65), "месяц");
        let (hours, days) = (ago(6 * 3600), ago(3 * 86_400));
        assert!(0.0 < hours && hours < 0.2 && 0.2 < days && days < 0.4, "{hours} {days}");
        assert_eq!(ago(365 * 86_400), 1.0);
        assert_eq!(ago(3 * 365 * 86_400), 1.0);
    }

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 байт");
        assert_eq!(size(1023), "1023 байт");
        assert_eq!(size(1536), "1,5 КБ");
        assert_eq!(size(824 * 1024 * 1024), "824 МБ");
        assert_eq!(size(3 * 1024u64.pow(4)), "3,0 ТБ");
        assert_eq!(size_column(10), "1 КБ");
    }

    #[test]
    fn plurals() {
        assert_eq!(items(1), "1 объект");
        assert_eq!(items(3), "3 объекта");
        assert_eq!(items(11), "11 объектов");
        assert_eq!(items(22), "22 объекта");
        assert_eq!(items(128), "128 объектов");
        assert_eq!(items(12_345), "12 345 объектов");
        assert_eq!(count(1_234_567), "1 234 567");
        assert_eq!(count(999), "999");
    }

    #[test]
    fn relative_dates() {
        let now = Local.with_ymd_and_hms(2026, 9, 19, 12, 0, 0).unwrap();
        let at = |d, h| SystemTime::from(Local.with_ymd_and_hms(2026, 9, d, h, 5, 0).unwrap());
        assert_eq!(date_relative_to(at(19, 1), now), "Сегодня 01:05");
        assert_eq!(date_relative_to(at(18, 14), now), "Вчера 14:05");
        assert_eq!(date_relative_to(at(1, 9), now), "01.09.2026 09:05");
        let old = SystemTime::from(Local.with_ymd_and_hms(2020, 1, 2, 3, 4, 5).unwrap());
        assert_eq!(date_relative_to(old, now), "02.01.2020");
    }
}
