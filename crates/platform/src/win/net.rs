//! HTTPS-запрос через WinHTTP: системный прокси (в том числе автонастройка), системное
//! хранилище сертификатов, распаковка gzip.

use std::ffi::c_void;

use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPTION_DECOMPRESSION, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetTimeouts,
};
use windows::core::{PCWSTR, w};

use super::com::wide;
use crate::net::TIMEOUT;

/// Описатель WinHTTP; закрывается при уничтожении.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, what: &str) -> Result<Handle, String> {
        if raw.is_null() {
            Err(format!("{what}: {}", windows::core::Error::from_thread()))
        } else {
            Ok(Handle(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: описатель получен от WinHTTP и закрывается один раз.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

pub fn get(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    limit: usize,
) -> Result<Vec<u8>, String> {
    let agent = wide(format!("MH-Files/{}", env!("CARGO_PKG_VERSION")));
    let host = wide(host);
    let path = wide(path);
    // Заголовки: «Имя: значение» через CRLF, без завершающего нуля.
    let headers: Vec<u16> = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>()
        .encode_utf16()
        .collect();
    let timeout = TIMEOUT.as_millis() as i32;
    // SAFETY: строки живут до конца функции, описатели закрываются в Drop в обратном порядке.
    unsafe {
        let session = Handle::new(
            WinHttpOpen(
                PCWSTR(agent.as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHTTP недоступен",
        )?;
        let _ = WinHttpSetTimeouts(session.0, timeout, timeout, timeout, timeout);
        // WINHTTP_DECOMPRESSION_FLAG_ALL: gzip и deflate.
        let _ = WinHttpSetOption(
            Some(session.0),
            WINHTTP_OPTION_DECOMPRESSION,
            Some(&3u32.to_ne_bytes()),
        );
        let connection = Handle::new(
            WinHttpConnect(session.0, PCWSTR(host.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0),
            "нет соединения",
        )?;
        let request = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "запрос не создан",
        )?;
        let headers = (!headers.is_empty()).then_some(headers.as_slice());
        WinHttpSendRequest(request.0, headers, None, 0, 0, 0)
            .map_err(|error| format!("запрос не отправлен: {error}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|error| format!("нет ответа: {error}"))?;
        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|error| format!("непонятный ответ: {error}"))?;
        if status != 200 {
            return Err(format!("сервер ответил кодом {status}"));
        }
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len() as u32,
                &mut read,
            )
            .map_err(|error| format!("ответ оборвался: {error}"))?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > limit {
                return Err("ответ слишком большой".into());
            }
        }
        Ok(body)
    }
}
