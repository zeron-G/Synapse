//! Cross-platform shared memory abstraction.
//!
//! - Linux: shm_open + mmap
//! - Windows: CreateFileMappingA + MapViewOfFile

use crate::error::{Result, SynapseError};

/// A mapped shared memory region.
pub struct SharedRegion {
    ptr: *mut u8,
    size: usize,
    name: String,
    is_creator: bool,
    #[cfg(windows)]
    handle: *mut ::core::ffi::c_void,
}

// SAFETY: SharedRegion is explicitly designed for cross-process shared memory.
// The pointer is valid for the lifetime of the region, and atomic operations
// are used for all concurrent access to the underlying data.
unsafe impl Send for SharedRegion {}
unsafe impl Sync for SharedRegion {}

impl SharedRegion {
    /// Create a new shared memory region.
    pub fn create(name: &str, size: usize) -> Result<Self> {
        #[cfg(unix)]
        { Self::unix_create(name, size) }
        #[cfg(windows)]
        { Self::win_create(name, size) }
    }

    /// Open an existing shared memory region.
    pub fn open(name: &str, size: usize) -> Result<Self> {
        #[cfg(unix)]
        { Self::unix_open(name, size) }
        #[cfg(windows)]
        { Self::win_open(name, size) }
    }

    /// Raw pointer to the mapped region.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// Size of the region.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Name of the region.
    pub fn name(&self) -> &str {
        &self.name
    }

    // ── Unix implementation ──

    #[cfg(unix)]
    fn unix_create(name: &str, size: usize) -> Result<Self> {
        use std::ffi::CString;
        let cname = CString::new(format!("/{name}"))
            .map_err(|e| SynapseError::ShmError(e.to_string()))?;

        unsafe {
            let fd = libc::shm_open(
                cname.as_ptr(),
                libc::O_CREAT | libc::O_RDWR | libc::O_EXCL,
                0o660,
            );
            if fd < 0 {
                return Err(SynapseError::ShmError(format!(
                    "shm_open create failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            if libc::ftruncate(fd, size as libc::off_t) != 0 {
                libc::close(fd);
                libc::shm_unlink(cname.as_ptr());
                return Err(SynapseError::ShmError(format!(
                    "ftruncate failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            let ptr = libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            libc::close(fd);

            if ptr == libc::MAP_FAILED {
                libc::shm_unlink(cname.as_ptr());
                return Err(SynapseError::ShmError(format!(
                    "mmap failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            // Zero the region
            std::ptr::write_bytes(ptr as *mut u8, 0, size);

            Ok(Self {
                ptr: ptr as *mut u8,
                size,
                name: name.to_string(),
                is_creator: true,
            })
        }
    }

    #[cfg(unix)]
    fn unix_open(name: &str, size: usize) -> Result<Self> {
        use std::ffi::CString;
        let cname = CString::new(format!("/{name}"))
            .map_err(|e| SynapseError::ShmError(e.to_string()))?;

        unsafe {
            let fd = libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0);
            if fd < 0 {
                return Err(SynapseError::ShmError(format!(
                    "shm_open open failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            let ptr = libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            libc::close(fd);

            if ptr == libc::MAP_FAILED {
                return Err(SynapseError::ShmError(format!(
                    "mmap failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            Ok(Self {
                ptr: ptr as *mut u8,
                size,
                name: name.to_string(),
                is_creator: false,
            })
        }
    }

    // ── Windows implementation ──
    // windows-sys 0.59: HANDLE = *mut c_void, MapViewOfFile → MEMORY_MAPPED_VIEW_ADDRESS

    #[cfg(windows)]
    fn win_create(name: &str, size: usize) -> Result<Self> {
        use windows_sys::Win32::System::Memory::*;
        use windows_sys::Win32::Foundation::*;

        let map_name = format!("Local\\synapse_{name}\0");

        unsafe {
            let handle = CreateFileMappingA(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                map_name.as_ptr(),
            );
            if handle.is_null() {
                return Err(SynapseError::ShmError(format!(
                    "CreateFileMappingA failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size);
            if view.Value.is_null() {
                CloseHandle(handle);
                return Err(SynapseError::ShmError(format!(
                    "MapViewOfFile failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            let ptr = view.Value as *mut u8;
            std::ptr::write_bytes(ptr, 0, size);

            Ok(Self {
                ptr,
                size,
                name: name.to_string(),
                is_creator: true,
                handle,
            })
        }
    }

    #[cfg(windows)]
    fn win_open(name: &str, size: usize) -> Result<Self> {
        use windows_sys::Win32::System::Memory::*;
        use windows_sys::Win32::Foundation::*;

        let map_name = format!("Local\\synapse_{name}\0");

        unsafe {
            let handle = OpenFileMappingA(FILE_MAP_ALL_ACCESS, 0, map_name.as_ptr());
            if handle.is_null() {
                return Err(SynapseError::ShmError(format!(
                    "OpenFileMappingA failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size);
            if view.Value.is_null() {
                CloseHandle(handle);
                return Err(SynapseError::ShmError(format!(
                    "MapViewOfFile failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            Ok(Self {
                ptr: view.Value as *mut u8,
                size,
                name: name.to_string(),
                is_creator: false,
                handle,
            })
        }
    }
}

impl Drop for SharedRegion {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.size);
            if self.is_creator {
                let cname = std::ffi::CString::new(format!("/{}", self.name)).unwrap();
                libc::shm_unlink(cname.as_ptr());
            }
        }

        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Memory::*;
            use windows_sys::Win32::Foundation::*;
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr as *mut _ });
            CloseHandle(self.handle);
        }
    }
}
