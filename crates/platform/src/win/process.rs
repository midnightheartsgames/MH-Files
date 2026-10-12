//! Задание Windows (job object) с «убить при закрытии»: процесс-помощник умирает вместе с
//! MH Files, как бы та ни завершилась.

use std::os::windows::io::AsRawHandle;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows::core::PCWSTR;

/// `CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS`: без консоли и не мешая интерфейсу.
pub const CREATION_FLAGS: u32 = 0x0800_0000 | 0x0000_4000;

pub struct Job(HANDLE);

// SAFETY: описатель задания можно закрыть из любого потока.
unsafe impl Send for Job {}

impl Job {
    /// Новое задание с процессом `child`. Не вышло — `None`: процесс работает и без него,
    /// просто не умрёт сам при сбое MH Files.
    pub fn adopt(child: &std::process::Child) -> Option<Job> {
        // SAFETY: структура заполнена нулями и размером; описатели закрываются в Drop.
        unsafe {
            let job = Job(CreateJobObjectW(None, PCWSTR::null()).ok()?);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .ok()?;
            AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).ok()?;
            Some(job)
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: описатель получен от CreateJobObjectW и закрывается один раз.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
