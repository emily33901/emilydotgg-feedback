use std::ffi::c_void;

#[derive(Debug, Clone, Copy)]
pub struct WindowHandle(isize);

impl WindowHandle {
    pub fn null() -> Self {
        Self(0)
    }

    pub fn is_valid(&self) -> bool {
        return self.0 != 0;
    }

    pub fn as_ptr(&self) -> Option<*mut c_void> {
        if self.is_valid() {
            Some(self.0 as *mut c_void)
        } else {
            None
        }
    }
    pub fn as_hwnd(&self) -> windows::Win32::Foundation::HWND {
        windows::Win32::Foundation::HWND(self.0)
    }
}

impl From<Option<*mut c_void>> for WindowHandle {
    fn from(value: Option<*mut c_void>) -> Self {
        Self(value.map(|v| v as isize).unwrap_or(0))
    }
}

impl From<*mut c_void> for WindowHandle {
    fn from(value: *mut c_void) -> Self {
        Self(value as isize)
    }
}
