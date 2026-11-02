//! Registration for the current user only (HKCU), so no administrator rights are needed and
//! nothing outside the user's own settings changes.

use crate::{CLSID, EXTENSIONS};
use std::path::Path;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW,
    RegSetKeyValueW,
};
use windows::core::{HSTRING, PCWSTR};

/// The shell's key for preview handlers under a file type.
const PREVIEW_HANDLER: &str = "{8895b1c6-b41f-4c1c-a562-0d564250836f}";
/// The preview host that runs 64-bit handlers (prevhost.exe).
const PREVHOST_APPID: &str = "{6d2b5079-2f0b-48dd-ab7f-97cec514d30b}";
const HANDLERS: &str = r"Software\Microsoft\Windows\CurrentVersion\PreviewHandlers";

fn check(e: WIN32_ERROR, what: &str) -> Result<(), String> {
    if e.is_ok() {
        Ok(())
    } else {
        Err(format!(
            "{what}: {}",
            windows::core::Error::from(e.to_hresult()).message()
        ))
    }
}

fn set(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    let data: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let name = name.map(HSTRING::from);
    // SAFETY: the strings live across the call; the data length is in bytes.
    let e = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    };
    check(e, key)
}

fn get(key: &str, name: Option<&str>) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let name = name.map(HSTRING::from);
    // SAFETY: `buf` holds `size` bytes.
    let e = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    e.is_ok().then(|| {
        let n = (size as usize / 2).saturating_sub(1);
        String::from_utf16_lossy(&buf[..n.min(buf.len())])
    })
}

/// Registers the handler DLL at `dll` for [`EXTENSIONS`], for the current user. A type the user
/// had given another preview handler gets Isolinear's, and that handler is remembered so that
/// [`unregister`] puts it back.
pub fn register(dll: &Path) -> Result<(), String> {
    if !dll.is_file() {
        return Err(format!("{} is missing", dll.display()));
    }
    let class = format!(r"Software\Classes\CLSID\{CLSID}");
    set(&class, None, "Isolinear preview handler")?;
    set(&class, Some("DisplayName"), "Isolinear")?;
    set(&class, Some("AppID"), PREVHOST_APPID)?;
    set(
        &format!(r"{class}\InprocServer32"),
        None,
        &dll.display().to_string(),
    )?;
    set(
        &format!(r"{class}\InprocServer32"),
        Some("ThreadingModel"),
        "Apartment",
    )?;
    set(HANDLERS, Some(CLSID), "Isolinear")?;
    for ext in EXTENSIONS {
        let key = format!(r"Software\Classes\.{ext}\shellex\{PREVIEW_HANDLER}");
        if let Some(previous) = get(&key, None).filter(|v| !v.eq_ignore_ascii_case(CLSID)) {
            set(&format!(r"{class}\Previous"), Some(ext), &previous)?;
        }
        set(&key, None, CLSID)?;
    }
    Ok(())
}

/// Removes what [`register`] wrote; file types now previewed by something else are left alone.
pub fn unregister() -> Result<(), String> {
    for ext in EXTENSIONS {
        let key = format!(r"Software\Classes\.{ext}\shellex\{PREVIEW_HANDLER}");
        if get(&key, None).is_some_and(|v| v.eq_ignore_ascii_case(CLSID)) {
            let class = format!(r"Software\Classes\CLSID\{CLSID}\Previous");
            match get(&class, Some(ext)) {
                Some(previous) => set(&key, None, &previous)?,
                None => {
                    // SAFETY: plain strings.
                    let e =
                        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key.as_str())) };
                    check(e, &key)?;
                }
            }
        }
    }
    // SAFETY: plain strings.
    let e = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(HANDLERS),
            &HSTRING::from(CLSID),
        )
    };
    if e != ERROR_FILE_NOT_FOUND {
        check(e, HANDLERS)?;
    }
    let class = format!(r"Software\Classes\CLSID\{CLSID}");
    // SAFETY: plain strings.
    let e = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(class.as_str())) };
    if e != ERROR_FILE_NOT_FOUND {
        check(e, &class)?;
    }
    Ok(())
}
