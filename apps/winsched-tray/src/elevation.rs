#![allow(unsafe_code)]

use std::mem::size_of;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: This wrapper exclusively owns the token handle.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

pub fn is_elevated() -> windows::core::Result<bool> {
    let mut token = HANDLE::default();
    // SAFETY: token is a valid output pointer and GetCurrentProcess returns a
    // pseudo-handle valid in this process.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token)? };
    let token = OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    // SAFETY: The information pointer and declared size match TOKEN_ELEVATION.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).expect("TOKEN_ELEVATION size fits u32"),
            &raw mut returned,
        )?;
    }
    Ok(elevation.TokenIsElevated != 0)
}
