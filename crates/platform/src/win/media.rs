//! Свойства файла через `IShellItem2::GetPropertyStore`. Значения форматирует сама Windows
//! (`PSFormatForDisplayAlloc`): единицы, даты и числа — на языке и в формате системы.

use std::path::Path;

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Storage::EnhancedStorage::{
    PKEY_Audio_ChannelCount, PKEY_Audio_EncodingBitrate, PKEY_Audio_SampleRate, PKEY_Author,
    PKEY_Company, PKEY_Document_PageCount, PKEY_Image_Dimensions, PKEY_Media_Duration,
    PKEY_Media_Year, PKEY_Music_AlbumArtist, PKEY_Music_AlbumTitle, PKEY_Music_Artist,
    PKEY_Music_Genre, PKEY_Photo_CameraManufacturer, PKEY_Photo_CameraModel, PKEY_Photo_DateTaken,
    PKEY_Photo_ExposureTime, PKEY_Photo_FNumber, PKEY_Photo_FocalLength, PKEY_Photo_ISOSpeed,
    PKEY_Title, PKEY_Video_EncodingBitrate, PKEY_Video_FrameHeight, PKEY_Video_FrameRate,
    PKEY_Video_FrameWidth, PKEY_Video_TotalBitrate,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PropVariantClear, PropVariantToUInt32,
};
use windows::Win32::System::Variant::VT_EMPTY;
use windows::Win32::UI::Shell::PropertiesSystem::{
    GPS_BESTEFFORT, GPS_DEFAULT, IPropertyStore, PDFF_DEFAULT, PSFormatForDisplayAlloc,
};
use windows::Win32::UI::Shell::{IShellItem2, SHCreateItemFromParsingName};
use windows::core::PCWSTR;

use super::com::{describe, wide};

/// Строка Инспектора.
enum Field {
    /// Подпись и ключи по порядку: показывается первый непустой.
    One(&'static str, &'static [PROPERTYKEY]),
    /// Ширина и высота кадра видео одной строкой.
    Frame,
}

/// Что и в каком порядке показывать. У файла обычно заполнена малая часть — остальное
/// пропускается.
const FIELDS: &[Field] = &[
    Field::One("Название", &[PKEY_Title]),
    Field::One("Длительность", &[PKEY_Media_Duration]),
    Field::Frame,
    Field::One("Размеры", &[PKEY_Image_Dimensions]),
    Field::One("Частота кадров", &[PKEY_Video_FrameRate]),
    // У видео — общий битрейт, у звука — битрейт дорожки.
    Field::One(
        "Битрейт",
        &[PKEY_Video_TotalBitrate, PKEY_Video_EncodingBitrate, PKEY_Audio_EncodingBitrate],
    ),
    Field::One("Дискретизация", &[PKEY_Audio_SampleRate]),
    Field::One("Каналы", &[PKEY_Audio_ChannelCount]),
    Field::One("Исполнитель", &[PKEY_Music_Artist, PKEY_Music_AlbumArtist]),
    Field::One("Альбом", &[PKEY_Music_AlbumTitle]),
    Field::One("Жанр", &[PKEY_Music_Genre]),
    Field::One("Год", &[PKEY_Media_Year]),
    // Модель обычно уже включает производителя; производитель — если модели нет.
    Field::One("Камера", &[PKEY_Photo_CameraModel, PKEY_Photo_CameraManufacturer]),
    Field::One("Снято", &[PKEY_Photo_DateTaken]),
    Field::One("Диафрагма", &[PKEY_Photo_FNumber]),
    Field::One("Выдержка", &[PKEY_Photo_ExposureTime]),
    Field::One("ISO", &[PKEY_Photo_ISOSpeed]),
    Field::One("Фокусное расстояние", &[PKEY_Photo_FocalLength]),
    Field::One("Страниц", &[PKEY_Document_PageCount]),
    Field::One("Автор", &[PKEY_Author]),
    Field::One("Организация", &[PKEY_Company]),
];

pub fn media_info(path: &Path) -> Result<Vec<(String, String)>, String> {
    let path_w = wide(path);
    // SAFETY: строка живёт до конца вызова.
    let item: IShellItem2 = unsafe { SHCreateItemFromParsingName(PCWSTR(path_w.as_ptr()), None) }
        .map_err(|error| describe(&path.display().to_string(), &error))?;
    // GPS_BESTEFFORT: что обработчик не смог прочитать, просто пусто, а не ошибка всего списка.
    // Медленные элементы (GPS_OPENSLOWITEM — офлайн-файлы) не открываются.
    // SAFETY: обычный вызов COM в потоке с апартаментом.
    let store: IPropertyStore = unsafe { item.GetPropertyStore(GPS_DEFAULT | GPS_BESTEFFORT) }
        .map_err(|error| describe("свойства файла не читаются", &error))?;

    let mut info = Vec::new();
    for field in FIELDS {
        match field {
            Field::One(label, keys) => {
                if let Some(text) = keys.iter().find_map(|key| display(&store, key)) {
                    info.push((label.to_string(), text));
                }
            }
            Field::Frame => {
                let width = number(&store, &PKEY_Video_FrameWidth);
                let height = number(&store, &PKEY_Video_FrameHeight);
                if let (Some(width), Some(height)) = (width, height) {
                    info.push(("Кадр".to_string(), format!("{width} × {height}")));
                }
            }
        }
    }
    Ok(info)
}

/// Значение свойства, как его показывает Проводник. `None` — пусто или не форматируется.
fn display(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    let value = Value::read(store, key)?;
    // SAFETY: ключ и значение живут до конца вызова; строку освобождает CoTaskMemFree ниже.
    let text = unsafe {
        let raw = PSFormatForDisplayAlloc(key, &value.0, PDFF_DEFAULT).ok()?;
        let text = raw.to_string();
        CoTaskMemFree(Some(raw.0 as *const _));
        text.ok()?
    };
    // Windows ставит в даты невидимые метки направления письма — в egui они видны квадратами.
    let text: String = text.chars().filter(|c| !matches!(c, '\u{200e}' | '\u{200f}')).collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Целое свойство (размер кадра). Ноль — то же, что пусто.
fn number(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<u32> {
    let value = Value::read(store, key)?;
    // SAFETY: значение живёт до конца вызова и только читается.
    let number = unsafe { PropVariantToUInt32(&value.0) }.ok()?;
    (number > 0).then_some(number)
}

/// PROPVARIANT из хранилища свойств; освобождается при уничтожении.
struct Value(PROPVARIANT);

impl Value {
    /// Непустое значение свойства или `None`.
    fn read(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<Value> {
        // SAFETY: ключ живёт до конца вызова; полученное значение освобождает Drop.
        let value = Value(unsafe { store.GetValue(key) }.ok()?);
        // SAFETY: поле vt есть у любого PROPVARIANT, объединение тут только читается.
        let empty = unsafe { value.0.Anonymous.Anonymous.vt } == VT_EMPTY;
        (!empty).then_some(value)
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        // SAFETY: значение получено из GetValue и принадлежит нам; после очистки не читается.
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}
