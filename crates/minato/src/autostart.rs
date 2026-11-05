//! "Start with Windows" via the per-user Run registry key.

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "MinatoWM";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn open(access: u32) -> Option<HKEY> {
    let mut key: HKEY = std::ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), 0, access, &mut key) };
    (rc == ERROR_SUCCESS).then_some(key)
}

pub fn is_enabled() -> bool {
    let Some(key) = open(KEY_QUERY_VALUE) else {
        return false;
    };
    let rc = unsafe {
        RegQueryValueExW(
            key,
            wide(VALUE).as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    unsafe { RegCloseKey(key) };
    rc == ERROR_SUCCESS
}

pub fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    let key = open(KEY_SET_VALUE).ok_or_else(|| anyhow::anyhow!("cannot open the Run key"))?;
    let rc = unsafe {
        if enabled {
            let exe = std::env::current_exe()?;
            let data = wide(&format!("\"{}\"", exe.display()));
            RegSetValueExW(
                key,
                wide(VALUE).as_ptr(),
                0,
                REG_SZ,
                data.as_ptr() as *const u8,
                (data.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(key, wide(VALUE).as_ptr())
        }
    };
    unsafe { RegCloseKey(key) };
    if rc != ERROR_SUCCESS && enabled {
        anyhow::bail!("writing the Run key failed ({rc})");
    }
    Ok(())
}
