use crate::OsInfo;
use objc2_foundation::{NSLocale, NSProcessInfo};
use std::ffi::CStr;

pub fn get_os_info() -> OsInfo {
    let process_info = NSProcessInfo::processInfo();
    let version = process_info.operatingSystemVersionString().to_string();
    let locale = NSLocale::currentLocale().localeIdentifier().to_string();

    let mut hostname_buf = [0u8; 256];
    let hostname = if unsafe {
        libc::gethostname(
            hostname_buf.as_mut_ptr() as *mut libc::c_char,
            hostname_buf.len(),
        )
    } == 0
    {
        hostname_buf[hostname_buf.len() - 1] = 0;
        unsafe { CStr::from_ptr(hostname_buf.as_ptr() as *const libc::c_char) }
            .to_string_lossy()
            .to_string()
    } else {
        String::new()
    };

    OsInfo {
        name: "macOS".into(),
        version: version.into(),
        arch: std::env::consts::ARCH.into(),
        locale: locale.into(),
        hostname: hostname.into(),
    }
}
