//! Раскладки клавиатуры: `GetKeyboardLayoutList` и имя языка по LANGID.

use windows::Win32::Globalization::LCIDToLocaleName;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayoutList, HKL};

pub fn keyboard_languages() -> Vec<String> {
    // SAFETY: первый вызов без буфера узнаёт число раскладок, второй пишет в свой буфер.
    let count = unsafe { GetKeyboardLayoutList(None) }.max(0) as usize;
    let mut layouts = vec![HKL::default(); count];
    let got = unsafe { GetKeyboardLayoutList(Some(&mut layouts)) }.max(0) as usize;
    let mut languages: Vec<String> = Vec::new();
    for layout in &layouts[..got.min(layouts.len())] {
        // Младшее слово HKL — LANGID; LCID с сортировкой по умолчанию равен ему же.
        let langid = (layout.0 as usize & 0xFFFF) as u32;
        let mut name = [0u16; 85];
        // SAFETY: буфер на стеке, его длина передаётся срезом.
        let len = unsafe { LCIDToLocaleName(langid, Some(&mut name), 0) };
        if len > 1 {
            let name = String::from_utf16_lossy(&name[..len as usize - 1]);
            if !languages.contains(&name) {
                languages.push(name);
            }
        }
    }
    languages
}
