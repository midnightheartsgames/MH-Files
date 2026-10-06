//! Эскизы (`IShellItemImageFactory`) и значки типов (`SHGetFileInfoW`, системные списки
//! значков). Результат — RGBA без премультипликации.

use std::path::Path;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES,
};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
    SHGFI_SMALLICON, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetImageList,
    SHIL_EXTRALARGE, SHIL_JUMBO, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::PCWSTR;

use super::com::{describe, wide};
use crate::thumbs::{Bitmap, ImageMode};

/// Сторона значка в списке SHIL_EXTRALARGE.
const EXTRALARGE: usize = 48;

pub fn shell_image(path: &Path, size: u32, mode: ImageMode) -> Result<Bitmap, String> {
    let path_w = wide(path);
    // SAFETY: строка живёт до конца вызова.
    let factory: IShellItemImageFactory =
        unsafe { SHCreateItemFromParsingName(PCWSTR(path_w.as_ptr()), None) }
            .map_err(|error| describe(&path.display().to_string(), &error))?;
    let flags = match mode {
        ImageMode::Thumbnail => SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK,
        ImageMode::Icon => SIIGBF_ICONONLY,
    };
    let side = size.clamp(1, 2560) as i32;
    // SAFETY: обычный вызов; полученный HBITMAP удаляет guard.
    let bitmap = unsafe { factory.GetImage(SIZE { cx: side, cy: side }, flags) }.map_err(
        |error| match mode {
            ImageMode::Thumbnail => describe("нет эскиза", &error),
            ImageMode::Icon => describe("нет значка", &error),
        },
    )?;
    let _bitmap = GdiObject(HGDIOBJ(bitmap.0));
    let (width, height, mut bgra) = read_bitmap(bitmap)?;
    opaque_if_no_alpha(&mut bgra);
    Ok(to_rgba(width, height, bgra, true))
}

pub fn type_icon(ext: &str, is_dir: bool, size: u32) -> Result<Bitmap, String> {
    // Файл не нужен: SHGFI_USEFILEATTRIBUTES берёт значок по имени и атрибутам.
    let (name, attributes) = if is_dir {
        ("folder".to_string(), FILE_ATTRIBUTE_DIRECTORY)
    } else if ext.is_empty() {
        ("file".to_string(), FILE_ATTRIBUTE_NORMAL)
    } else {
        (format!("file.{ext}"), FILE_ATTRIBUTE_NORMAL)
    };
    let name = wide(name);
    if size > 32
        && let Ok(bitmap) = list_icon(&name, attributes, size as usize)
    {
        return Ok(bitmap);
    }

    let mut info = SHFILEINFOW::default();
    let flags = SHGFI_USEFILEATTRIBUTES
        | SHGFI_ICON
        | if size <= 16 { SHGFI_SMALLICON } else { SHGFI_LARGEICON };
    // SAFETY: структура и строка живут до конца вызова; значок удаляет guard.
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(name.as_ptr()),
            attributes,
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if ok == 0 || info.hIcon.is_invalid() {
        return Err("Windows не дала значок типа".into());
    }
    let _icon = Icon(info.hIcon);
    icon_to_bitmap(info.hIcon)
}

/// Крупный значок из системного списка: 48 точек или 256 (jumbo).
fn list_icon(
    name: &[u16],
    attributes: FILE_FLAGS_AND_ATTRIBUTES,
    size: usize,
) -> Result<Bitmap, String> {
    let mut info = SHFILEINFOW::default();
    // SAFETY: структура и строка живут до конца вызова. Системный список значков не наш, его
    // не освобождают.
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(name.as_ptr()),
            attributes,
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_USEFILEATTRIBUTES | SHGFI_SYSICONINDEX,
        )
    };
    if ok == 0 {
        return Err("нет индекса значка".into());
    }
    let from_list = |list: u32| -> Result<Bitmap, String> {
        // SAFETY: значок из списка — наша копия, её удаляет guard.
        unsafe {
            let list: IImageList = SHGetImageList(list as i32)
                .map_err(|error| describe("нет списка значков", &error))?;
            let icon = list
                .GetIcon(info.iIcon, ILD_TRANSPARENT.0)
                .map_err(|error| describe("нет значка в списке", &error))?;
            let _icon = Icon(icon);
            icon_to_bitmap(icon)
        }
    };
    if size > EXTRALARGE {
        // У типа без своего значка 256 точек jumbo — это значок 48 точек в углу пустого поля:
        // тогда честнее взять 48 и дать вызывающему масштабировать.
        if let Ok(bitmap) = from_list(SHIL_JUMBO)
            && !only_top_left(&bitmap, EXTRALARGE)
        {
            return Ok(bitmap);
        }
    }
    from_list(SHIL_EXTRALARGE)
}

/// Всё видимое умещается в левом верхнем квадрате `side`.
fn only_top_left(bitmap: &Bitmap, side: usize) -> bool {
    let width = bitmap.width as usize;
    if width <= side && bitmap.height as usize <= side {
        return false;
    }
    bitmap.rgba.chunks_exact(4).enumerate().all(|(i, px)| {
        let (x, y) = (i % width, i / width);
        (x < side && y < side) || px[3] == 0
    })
}

/// HICON в картинку. Цветной слой 32 бита с альфой берётся как есть, без альфы — с маской,
/// монохромный значок собирается из масок AND/XOR.
fn icon_to_bitmap(icon: HICON) -> Result<Bitmap, String> {
    let mut info = ICONINFO::default();
    // SAFETY: GetIconInfo создаёт копии слоёв; их удаляют guard'ы ниже.
    unsafe { GetIconInfo(icon, &mut info) }
        .map_err(|error| describe("значок не читается", &error))?;
    let _color = (!info.hbmColor.is_invalid()).then(|| GdiObject(HGDIOBJ(info.hbmColor.0)));
    let _mask = (!info.hbmMask.is_invalid()).then(|| GdiObject(HGDIOBJ(info.hbmMask.0)));

    if !info.hbmColor.is_invalid() {
        let (width, height, mut bgra) = read_bitmap(info.hbmColor)?;
        if bgra.chunks_exact(4).all(|px| px[3] == 0) {
            // Значок без альфы: прозрачность только в маске (белое — прозрачно).
            match read_bitmap(info.hbmMask) {
                Ok((mw, mh, mask)) if mw == width && mh >= height => {
                    for (px, m) in bgra.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                        px[3] = if m[0] == 0 { 255 } else { 0 };
                    }
                }
                _ => opaque_if_no_alpha(&mut bgra),
            }
        }
        // У значков альфа прямая, не премультиплицированная.
        return Ok(to_rgba(width, height, bgra, false));
    }

    // Монохромный: маска двойной высоты, сверху AND, снизу XOR.
    let (width, double, mask) = read_bitmap(info.hbmMask)?;
    let height = double / 2;
    if height == 0 {
        return Err("пустой значок".into());
    }
    let half = (width * height * 4) as usize;
    let (and, xor) = mask.split_at(half);
    let mut bgra = vec![0u8; half];
    for ((px, a), x) in bgra.chunks_exact_mut(4).zip(and.chunks_exact(4)).zip(xor.chunks_exact(4)) {
        let transparent = a[0] != 0;
        let white = x[0] != 0;
        match (transparent, white) {
            (true, false) => {}
            // «Инверсия фона» — рисуем чёрным.
            (true, true) => px.copy_from_slice(&[0, 0, 0, 255]),
            (false, white) => {
                let c = if white { 255 } else { 0 };
                px.copy_from_slice(&[c, c, c, 255]);
            }
        }
    }
    Ok(to_rgba(width, height, bgra, false))
}

/// Пиксели HBITMAP: 32 бита BGRA, строки сверху вниз.
fn read_bitmap(bitmap: HBITMAP) -> Result<(u32, u32, Vec<u8>), String> {
    let mut header = BITMAP::default();
    // SAFETY: в структуру пишется не больше её размера.
    let got = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut header as *mut BITMAP as *mut _),
        )
    };
    let (width, height) = (header.bmWidth, header.bmHeight.abs());
    if got == 0 || width <= 0 || height <= 0 {
        return Err("картинка не читается".into());
    }
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Отрицательная высота — строки сверху вниз.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: буфер ровно на width × height пикселей по 4 байта; DC удаляет guard.
    let lines = unsafe {
        let dc = MemDc(CreateCompatibleDC(None));
        if dc.0.is_invalid() {
            return Err("нет контекста рисования".into());
        }
        GetDIBits(
            dc.0,
            bitmap,
            0,
            height as u32,
            Some(bgra.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if lines != height {
        return Err("пиксели картинки не читаются".into());
    }
    Ok((width as u32, height as u32, bgra))
}

/// Картинка без альфы (все нули) — непрозрачная.
fn opaque_if_no_alpha(bgra: &mut [u8]) {
    if bgra.chunks_exact(4).all(|px| px[3] == 0) {
        bgra.chunks_exact_mut(4).for_each(|px| px[3] = 255);
    }
}

/// BGRA → RGBA; премультиплицированные цвета делятся обратно на альфу.
fn to_rgba(width: u32, height: u32, mut px: Vec<u8>, premultiplied: bool) -> Bitmap {
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
        let a = p[3] as u32;
        if premultiplied && a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Bitmap { width, height, rgba: px }
}

struct GdiObject(HGDIOBJ);

impl Drop for GdiObject {
    fn drop(&mut self) {
        // SAFETY: объект наш и больше нигде не выбран.
        let _ = unsafe { DeleteObject(self.0) };
    }
}

struct Icon(HICON);

impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: значок создан для нас (SHGetFileInfoW, IImageList::GetIcon).
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

struct MemDc(HDC);

impl Drop for MemDc {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: DC создан CreateCompatibleDC.
            let _ = unsafe { DeleteDC(self.0) };
        }
    }
}
