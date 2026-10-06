//! PDF через WinRT `Windows.Data.Pdf`: файл → документ → страница → PNG в памяти.
//!
//! Асинхронные вызовы WinRT здесь дожидаются блокирующим `join`. В STA так ждать нельзя:
//! ожидание не крутит сообщения, а объект может захотеть вернуться в этот апартамент — поток
//! зависнет. Поэтому из STA (воркеры эскизов и предпросмотра) работа уходит в короткий поток
//! MTA.

use std::path::Path;

use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Storage::StorageFile;
use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
use windows::Win32::Foundation::ERROR_WRONG_PASSWORD;
use windows::Win32::System::Com::{
    APTTYPE, APTTYPE_MTA, APTTYPEQUALIFIER, COINIT_MULTITHREADED, CoGetApartmentType,
    CoInitializeEx, CoUninitialize,
};
use windows::core::{HRESULT, HSTRING};

use super::com::describe;
use crate::pdf::fit;

/// Начало любого файла PNG.
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

pub fn render_page(path: &Path, page: u32, max_side: u32) -> Result<(Vec<u8>, u32), String> {
    if in_mta() {
        return render(path, page, max_side);
    }
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name("pdf-render".into())
        .spawn(move || {
            let _apartment = Mta::enter();
            render(&path, page, max_side)
        })
        .map_err(|error| format!("поток отрисовки PDF не запущен: {error}"))?
        .join()
        .unwrap_or_else(|_| Err("отрисовка PDF прервалась".into()))
}

fn render(path: &Path, page: u32, max_side: u32) -> Result<(Vec<u8>, u32), String> {
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path))
        .and_then(|operation| operation.join())
        .map_err(|error| describe(&path.display().to_string(), &error))?;
    let document = PdfDocument::LoadFromFileAsync(&file)
        .and_then(|operation| operation.join())
        .map_err(|error| {
            if error.code() == HRESULT::from_win32(ERROR_WRONG_PASSWORD.0) {
                "PDF защищён паролем — откройте его в программе для PDF".to_string()
            } else {
                describe("файл не открывается как PDF", &error)
            }
        })?;
    let count = document.PageCount().map_err(|error| describe("PDF не читается", &error))?;
    if count == 0 {
        return Err("в PDF нет страниц".into());
    }

    let page = document
        .GetPage(page.min(count - 1))
        .map_err(|error| describe("страница PDF не читается", &error))?;
    let png = render_to_png(&page, max_side);
    // Страница держит часть документа в памяти; закрыть сразу, не дожидаясь сборки.
    let _ = page.Close();
    Ok((png?, count))
}

fn render_to_png(page: &windows::Data::Pdf::PdfPage, max_side: u32) -> Result<Vec<u8>, String> {
    let failed = |error: windows::core::Error| describe("страница PDF не отрисовалась", &error);
    // Размер уже с учётом поворота страницы и её CropBox.
    let size = page.Size().map_err(failed)?;
    let (width, height) = fit(size.Width, size.Height, max_side);
    let options = PdfPageRenderOptions::new().map_err(failed)?;
    options.SetDestinationWidth(width).map_err(failed)?;
    options.SetDestinationHeight(height).map_err(failed)?;

    // Кодировщик по умолчанию — PNG.
    let stream = InMemoryRandomAccessStream::new().map_err(failed)?;
    page.RenderWithOptionsToStreamAsync(&stream, &options)
        .and_then(|action| action.join())
        .map_err(failed)?;

    let length = stream.Size().map_err(failed)?;
    let length = u32::try_from(length).map_err(|_| "страница PDF слишком большая".to_string())?;
    let reader = stream
        .GetInputStreamAt(0)
        .and_then(|input| DataReader::CreateDataReader(&input))
        .map_err(failed)?;
    let loaded = reader.LoadAsync(length).and_then(|operation| operation.join()).map_err(failed)?;
    let mut png = vec![0u8; loaded as usize];
    reader.ReadBytes(&mut png).map_err(failed)?;
    if !png.starts_with(PNG_SIGNATURE) {
        return Err("страница PDF отрисована не в PNG".into());
    }
    Ok(png)
}

/// Поток уже в MTA (своём или неявном общем процесса): WinRT можно ждать прямо здесь.
fn in_mta() -> bool {
    let mut kind = APTTYPE::default();
    let mut qualifier = APTTYPEQUALIFIER::default();
    // SAFETY: обе переменные живут до конца вызова; функция только читает состояние потока.
    unsafe { CoGetApartmentType(&mut kind, &mut qualifier) }.is_ok() && kind == APTTYPE_MTA
}

/// COM MTA вспомогательного потока.
struct Mta {
    /// `CoUninitialize` — только в паре с успешным `CoInitializeEx`.
    initialized: bool,
}

impl Mta {
    fn enter() -> Mta {
        // SAFETY: поток наш и новый; парный вызов — в Drop.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Mta { initialized: hr.is_ok() }
    }
}

impl Drop for Mta {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: парный вызов к успешному CoInitializeEx в этом же потоке; все объекты
            // WinRT этого потока к этому времени уже освобождены.
            unsafe { CoUninitialize() };
        }
    }
}
