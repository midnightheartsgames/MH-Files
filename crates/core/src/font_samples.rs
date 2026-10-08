//! Образцы текста для просмотра шрифтов: по панграмме (или алфавиту) на язык. Языки берутся
//! из раскладок клавиатуры Windows — шрифт показывается на тех языках, на которых пишут.

/// Цифры и знаки — строка под образцами.
pub const DIGITS: &str = "0123456789 .,:;!? «»\"'()[]{} @#$%&*+-=/ №";

/// Образец для языка: русское название и текст. `locale` — BCP 47 («en-US», «ru-RU»,
/// «sr-Latn-RS»), регистр не важен. `None` — языка нет в таблице.
pub fn sample(locale: &str) -> Option<(&'static str, &'static str)> {
    let locale = locale.to_ascii_lowercase();
    let mut parts = locale.split(['-', '_']);
    let language = parts.next().unwrap_or_default();
    let latin_serbian = locale.split(['-', '_']).any(|part| part == "latn");
    Some(match language {
        "en" => ("Английский", "The quick brown fox jumps over the lazy dog"),
        "ru" => ("Русский", "Съешь же ещё этих мягких французских булок, да выпей чаю"),
        "uk" => ("Украинский", "Чуєш їх, доцю, га? Кумедна ж ти, прощайся без ґольфів!"),
        "be" => {
            ("Белорусский", "У рудога вераб'я ў сховішчы пад фатэлем ляжаць нейкія гаючыя зёлкі")
        }
        "bg" => ("Болгарский", "Ах, чудна българска земьо, полюшвай цъфтящи жита"),
        "sr" if latin_serbian => {
            ("Сербский (латиница)", "Ljubazni fenjerdžija čađavog lica hoće da mi pokaže štos")
        }
        "sr" => ("Сербский", "Љубазни фењерџија чађавог лица хоће да ми покаже штос"),
        "hr" | "bs" => {
            ("Хорватский", "Gojazni đačić s biciklom drži hmelj i finu vatu u džepu nošnje")
        }
        "de" => ("Немецкий", "Victor jagt zwölf Boxkämpfer quer über den großen Sylter Deich"),
        "fr" => {
            ("Французский", "Voix ambiguë d'un cœur qui, au zéphyr, préfère les jattes de kiwis")
        }
        "es" => ("Испанский", "El veloz murciélago hindú comía feliz cardillo y kiwi"),
        "it" => (
            "Итальянский",
            "Quel vituperabile xenofobo zelante assaggia il whisky ed esclama: alleluja!",
        ),
        "pt" => (
            "Португальский",
            "Luís argüia à Júlia que «brações, fé, chá, óxido, pôr, zângão» eram palavras",
        ),
        "pl" => ("Польский", "Pchnąć w tę łódź jeża lub ośm skrzyń fig"),
        "cs" => ("Чешский", "Příliš žluťoučký kůň úpěl ďábelské ódy"),
        "sk" => {
            ("Словацкий", "Kŕdeľ šťastných ďatľov učí pri ústí Váhu mĺkveho koňa obhrýzať kôru")
        }
        "tr" => ("Турецкий", "Pijamalı hasta yağız şoföre çabucak güvendi"),
        "nl" => ("Нидерландский", "Pa's wijze lynx bezag vroom het fikse aquaduct"),
        "sv" => ("Шведский", "Flygande bäckasiner söka hwila på mjuka tuvor"),
        "da" => ("Датский", "Høj bly gom vandt fræk sexquiz på wc"),
        "nb" | "nn" | "no" => {
            ("Норвежский", "Vår sære Zulu fra badeøya spilte jo whist og quickstep i min taxi")
        }
        "fi" => ("Финский", "Törkylempijävongahdus"),
        "hu" => ("Венгерский", "Árvíztűrő tükörfúrógép"),
        "ro" => ("Румынский", "Muzicologă în bej vând whisky și tequila, preț fix"),
        "lt" => ("Литовский", "Įlinkdama fechtuotojo špaga sublykčiojusi pragręžė apvalų arbūzą"),
        "lv" => ("Латышский", "Glāžšķūņa rūķīši dzērumā čiepj Baha koncertflīģeļu vākus"),
        "et" => {
            ("Эстонский", "Põdur Zagrebi tšellomängija-följetonist Ciqo külmetas kehvas garaažis")
        }
        "el" => ("Греческий", "Ξεσκεπάζω την ψυχοφθόρα βδελυγμία"),
        "kk" => (
            "Казахский",
            "А Ә Б В Г Ғ Д Е Ё Ж З И Й К Қ Л М Н Ң О Ө П Р С Т У Ұ Ү Ф Х Һ Ц Ч Ш Щ Ъ Ы І Ь Э Ю Я",
        ),
        "ka" => ("Грузинский", "ა ბ გ დ ე ვ ზ თ ი კ ლ მ ნ ო პ ჟ რ ს ტ უ ფ ქ ღ ყ შ ჩ ც ძ წ ჭ ხ ჯ ჰ"),
        "hy" => (
            "Армянский",
            "Ա Բ Գ Դ Ե Զ Է Ը Թ Ժ Ի Լ Խ Ծ Կ Հ Ձ Ղ Ճ Մ Յ Ն Շ Ո Չ Պ Ջ Ռ Ս Վ Տ Ր Ց Ւ Փ Ք Օ Ֆ",
        ),
        "he" => ("Иврит", "דג סקרן שט בים מאוכזב ולפתע מצא חברה"),
        "ar" => ("Арабский", "نص حكيم له سر قاطع وذو شأن عظيم مكتوب على ثوب أخضر"),
        "ko" => ("Корейский", "키스의 고유조건은 입술끼리 만나야 하고 특별한 기술은 필요치 않다"),
        "ja" => ("Японский", "いろはにほへと ちりぬるを わかよたれそ つねならむ"),
        "zh" => ("Китайский", "天地玄黄，宇宙洪荒，日月盈昃，辰宿列张"),
        _ => return None,
    })
}

/// Образцы для списка языков раскладок: без повторов одного языка; ни одного не нашлось —
/// английский.
pub fn samples(locales: &[String]) -> Vec<(&'static str, &'static str)> {
    let mut found: Vec<(&str, &str)> = Vec::new();
    for locale in locales {
        if let Some(sample) = sample(locale)
            && !found.contains(&sample)
        {
            found.push(sample);
        }
    }
    if found.is_empty() {
        found.extend(sample("en"));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_language_and_script() {
        assert_eq!(sample("ru-RU").unwrap().0, "Русский");
        assert_eq!(sample("EN-us").unwrap().0, "Английский");
        assert_eq!(sample("sr-Latn-RS").unwrap().0, "Сербский (латиница)");
        assert_eq!(sample("sr-Cyrl-RS").unwrap().0, "Сербский");
        assert!(sample("xx-YY").is_none());
    }

    #[test]
    fn samples_dedupe_and_fall_back_to_english() {
        let list = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let names: Vec<&str> =
            samples(&list(&["en-US", "ru-RU", "en-GB"])).iter().map(|s| s.0).collect();
        assert_eq!(names, ["Английский", "Русский"]);
        assert_eq!(samples(&list(&["xx"]))[0].0, "Английский");
    }
}
