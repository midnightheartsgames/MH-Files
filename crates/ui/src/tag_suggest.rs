//! Подсказки тегов под полем ввода: в поиске по тегам — для любого слова, в фильтре папки —
//! для слов `tag:`. Теги — те, что уже встречаются у посчитанных файлов (самые частые выше),
//! с числом файлов. Стрелки выбирают, Enter или щелчок подставляют, Esc прячет.

use eframe::egui::{
    self, Align2, Id, Key, Modifiers, Rect, Response, RichText, Ui, pos2, text::CCursor,
    text::CCursorRange, vec2,
};

use crate::theme;

/// Подсказок в списке не больше.
const MAX: usize = 8;

#[derive(Clone, Default)]
struct State {
    selected: usize,
    /// Для какого обрывка выбран пункт: другой обрывок — выбор с начала.
    fragment: String,
    /// Esc спрятал подсказки для этого текста.
    dismissed: Option<String>,
    /// Где был список в прошлом кадре: щелчок по нему не должен его закрыть.
    rect: Option<Rect>,
}

/// Слово под курсором: начало, конец (в символах), приставка (`-`, `tag:`, предыдущие
/// варианты через `|`) и обрывок тега.
struct Word {
    start: usize,
    end: usize,
    prefix: String,
    fragment: String,
}

/// Слово под курсором `cursor` (в символах). `bare` — голые слова тоже теги; иначе — только
/// `tag:…`.
fn word_at(text: &str, cursor: usize, bare: bool) -> Option<Word> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let start = chars[..cursor].iter().rposition(|c| c.is_whitespace()).map_or(0, |i| i + 1);
    let end =
        chars[cursor..].iter().position(|c| c.is_whitespace()).map_or(chars.len(), |i| cursor + i);
    let word: String = chars[start..end].iter().collect();
    let mut rest = word.as_str();
    let mut prefix = String::new();
    if let Some(after) = rest.strip_prefix('-') {
        prefix.push('-');
        rest = after;
    }
    let lower = rest.to_lowercase();
    if lower.starts_with("tag:") {
        prefix.push_str(&rest[..4]);
        rest = &rest[4..];
    } else if !bare || lower.contains(':') {
        return None;
    }
    // `cat|do` — подсказывается последний вариант.
    if let Some(bar) = rest.rfind('|') {
        prefix.push_str(&rest[..=bar]);
        rest = &rest[bar + 1..];
    }
    let fragment = rest.trim_matches('"').to_string();
    (!fragment.is_empty()).then_some(Word { start, end, prefix, fragment })
}

/// Подходящие теги: с начала имени, потом с начала слова в имени, потом где угодно; внутри —
/// по числу файлов.
fn candidates(known: &[(String, u32)], fragment: &str) -> Vec<(String, u32)> {
    let needle = mh_files_core::tags::display_name(fragment).to_lowercase();
    let mut found: Vec<(u8, &(String, u32))> = known
        .iter()
        .filter_map(|tag| {
            let name = tag.0.to_lowercase();
            let rank = if name.starts_with(&needle) {
                0
            } else if name.split([' ', '(', '-']).any(|w| w.starts_with(&needle)) {
                1
            } else if name.contains(&needle) {
                2
            } else {
                return None;
            };
            Some((rank, tag))
        })
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().take(MAX).map(|(_, tag)| tag.clone()).collect()
}

/// Показать подсказки под полем `response` (с id `id`) и подставить выбранную. Звать сразу
/// после поля — до того, как стрелки и Enter прочитает кто-то ещё. Возвращает новый текст,
/// если подсказка подставлена.
pub fn show(
    ui: &mut Ui,
    id: Id,
    response: &Response,
    text: &str,
    bare: bool,
    tagger: &mh_files_fs::Tagger,
) -> Option<String> {
    let ctx = ui.ctx().clone();
    let state_id = id.with("tag-suggest");
    let mut state: State = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
    let pointer_on_list =
        state.rect.zip(ctx.pointer_hover_pos()).is_some_and(|(rect, pos)| rect.contains(pos));
    let entered = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    if !response.has_focus() && !pointer_on_list && !entered {
        if state.rect.take().is_some() {
            ctx.data_mut(|d| d.insert_temp(state_id, state));
        }
        return None;
    }
    let cursor = egui::TextEdit::load_state(&ctx, id)
        .and_then(|s| s.cursor.char_range())
        .map_or(text.chars().count(), |r| r.primary.index.into());
    let word = word_at(text, cursor, bare);
    let found = match (&word, tagger.known_tags()) {
        (Some(word), Some(known)) => candidates(&known, &word.fragment),
        _ => Vec::new(),
    };
    let only_itself = found.len() == 1
        && mh_files_core::tags::query_token(&found[0].0)
            .eq_ignore_ascii_case(&word.as_ref().map(|w| w.fragment.clone()).unwrap_or_default());
    if state.dismissed.as_deref().is_some_and(|t| t != text) {
        state.dismissed = None;
    }
    let Some(word) =
        word.filter(|_| !found.is_empty() && !only_itself && state.dismissed.is_none())
    else {
        state.rect = None;
        ctx.data_mut(|d| d.insert_temp(state_id, state));
        return None;
    };
    if state.fragment != word.fragment {
        state.fragment = word.fragment.clone();
        state.selected = 0;
    }
    state.selected = state.selected.min(found.len() - 1);
    let (down, up, escape) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Escape),
        )
    });
    if down {
        state.selected = (state.selected + 1) % found.len();
    }
    if up {
        state.selected = (state.selected + found.len() - 1) % found.len();
    }
    if escape {
        state.dismissed = Some(text.to_string());
        state.rect = None;
        ctx.data_mut(|d| d.insert_temp(state_id, state));
        return None;
    }
    let mut picked = None;
    if entered {
        // Enter у однострочного поля снимает фокус: подсказка подставляется, фокус — назад.
        ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        picked = Some(state.selected);
    }
    let width = response.rect.width().clamp(240.0, 420.0);
    let area = egui::Area::new(state_id.with("list"))
        .order(egui::Order::Foreground)
        .fixed_pos(response.rect.left_bottom() + vec2(0.0, 4.0))
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 0.0;
                for (index, (name, count)) in found.iter().enumerate() {
                    let (rect, row) =
                        ui.allocate_exact_size(vec2(width, 24.0), egui::Sense::click());
                    if row.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                        state.selected = index;
                    }
                    if index == state.selected {
                        ui.painter().rect_filled(rect, 4.0, theme::accent().gamma_multiply(0.25));
                    }
                    ui.painter().text(
                        pos2(rect.left() + 8.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        name,
                        theme::regular(13.5),
                        theme::TEXT_PRIMARY,
                    );
                    ui.painter().text(
                        pos2(rect.right() - 8.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        mh_files_core::format::count(*count as usize),
                        theme::regular(12.5),
                        theme::TEXT_SECONDARY,
                    );
                    if row.clicked() {
                        picked = Some(index);
                    }
                }
                ui.add_space(2.0);
                ui.label(
                    RichText::new("стрелки — выбрать · Enter — подставить · Esc — скрыть")
                        .font(theme::regular(11.5))
                        .color(theme::TEXT_DISABLED),
                );
            });
        });
    state.rect = Some(area.response.rect);
    let result = picked.and_then(|index| found.get(index)).map(|(name, _)| {
        let chars: Vec<char> = text.chars().collect();
        let before: String = chars[..word.start].iter().collect();
        let after: String = chars[word.end..].iter().collect();
        let token = format!("{}{} ", word.prefix, mh_files_core::tags::query_token(name));
        let cursor = before.chars().count() + token.chars().count();
        let new_text = format!("{before}{token}{}", after.trim_start());
        // Курсор — после подставленного тега и пробела: можно писать следующий.
        let mut edit = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
        edit.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(cursor))));
        edit.store(&ctx, id);
        response.request_focus();
        state.rect = None;
        new_text
    });
    ctx.data_mut(|d| d.insert_temp(state_id, state));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_word_under_the_cursor() {
        let word = word_at("cat out", 7, true).unwrap();
        assert_eq!(
            (word.start, word.end, word.prefix.as_str(), word.fragment.as_str()),
            (4, 7, "", "out")
        );
        let word = word_at("отпуск -tag:do", 14, false).unwrap();
        assert_eq!((word.prefix.as_str(), word.fragment.as_str()), ("-tag:", "do"));
        assert!(word_at("отпуск", 6, false).is_none(), "в фильтре — только tag:");
        assert!(word_at("rating:gen", 10, true).is_none(), "рейтинги — плашками");
        let word = word_at("cat|do x", 6, true).unwrap();
        assert_eq!((word.prefix.as_str(), word.fragment.as_str(), word.end), ("cat|", "do", 6));
        assert!(word_at("cat ", 4, true).is_none(), "после пробела — нового слова ещё нет");
    }

    #[test]
    fn ranks_candidates() {
        let known: Vec<(String, u32)> =
            [("outdoors", 50), ("long hair", 40), ("hair ornament", 5), ("chair", 30)]
                .map(|(n, c)| (n.to_string(), c))
                .to_vec();
        let names = |f: &str| candidates(&known, f).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(names("hair"), vec!["hair ornament", "long hair", "chair"]);
        assert_eq!(names("long_h"), vec!["long hair"]);
        assert!(names("xyz").is_empty());
    }
}
