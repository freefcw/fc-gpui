use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as _, Result};
use util::ResultExt;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Power::{
                ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, EXECUTION_STATE,
                PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
                SetThreadExecutionState,
            },
            SystemInformation::GetTickCount,
            SystemServices::POWER_REQUEST_CONTEXT_VERSION,
            Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
        },
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    },
    core::PWSTR,
};

use crate::PowerSaveBlockerKind;

pub(crate) struct PowerRequest {
    handle: HANDLE,
}

unsafe impl Send for PowerRequest {}

impl PowerRequest {
    pub(crate) fn prevent_idle_sleep(reason: &str) -> Result<Self> {
        let mut reason = reason.encode_utf16().chain([0]).collect::<Vec<_>>();
        let context = REASON_CONTEXT {
            Version: POWER_REQUEST_CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: PWSTR(reason.as_mut_ptr()),
            },
        };
        let handle = unsafe { PowerCreateRequest(&context) }
            .context("Failed to create a Windows power request")?;
        if let Err(error) = unsafe { PowerSetRequest(handle, PowerRequestSystemRequired) } {
            unsafe { CloseHandle(handle) }
                .context("Failed to close the Windows power request")
                .log_err();
            return Err(error).context("Failed to set the Windows power request");
        }
        Ok(Self { handle })
    }
}

impl Drop for PowerRequest {
    fn drop(&mut self) {
        unsafe { PowerClearRequest(self.handle, PowerRequestSystemRequired) }
            .context("Failed to clear the Windows power request")
            .log_err();
        unsafe { CloseHandle(self.handle) }
            .context("Failed to close the Windows power request")
            .log_err();
    }
}

pub(crate) fn power_save_flags(kind: PowerSaveBlockerKind) -> EXECUTION_STATE {
    match kind {
        PowerSaveBlockerKind::PreventAppSuspension => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
        PowerSaveBlockerKind::PreventDisplaySleep => {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
        }
    }
}

pub(crate) fn apply_combined_power_state(blockers: &HashMap<u32, EXECUTION_STATE>) {
    let combined = blockers
        .values()
        .fold(ES_CONTINUOUS, |acc, &flags| acc | flags);
    unsafe {
        SetThreadExecutionState(combined);
    }
}

pub(crate) fn system_idle_time() -> Option<Duration> {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    let success = unsafe { GetLastInputInfo(&mut info) };
    if success.as_bool() {
        let now = unsafe { GetTickCount() };
        let idle_ms = now.wrapping_sub(info.dwTime);
        Some(Duration::from_millis(idle_ms as u64))
    } else {
        None
    }
}
