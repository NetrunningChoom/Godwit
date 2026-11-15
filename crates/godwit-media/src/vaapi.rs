// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Optional VA-API capability discovery. The ABI values and call contracts
//! match Intel's public libva headers; DRM display opening follows the
//! Apache-2.0 webrtc-sys 0.3.52 `src/vaapi/vaapi_display_drm.cpp` pattern.

/// Codecs for which the selected VA display has a VLD decode entrypoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Codecs {
    pub h264: bool,
    pub vp8: bool,
    pub vp9: bool,
    pub av1: bool,
}

/// Result of probing one DRM render node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapabilityReport {
    pub codecs: Codecs,
    pub vendor: Option<String>,
}

/// Queries a DRM render node without connecting to a display server.
/// Missing libraries, devices, symbols, or unsupported drivers return an empty
/// report so callers can keep using the software decoder.
#[cfg(target_os = "linux")]
pub fn probe() -> CapabilityReport {
    linux::probe()
}

/// VA-API has no adapter on other operating systems.
#[cfg(not(target_os = "linux"))]
pub fn probe() -> CapabilityReport {
    CapabilityReport::default()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{CapabilityReport, Codecs};
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    const RTLD_NOW: c_int = 2;
    const RTLD_LOCAL: c_int = 0;
    const SUCCESS: c_int = 0;
    const VLD: c_int = 1;
    const MAX_ITEMS: usize = 4096;
    const PROFILES: &[(c_int, u8)] = &[
        (5, 0),
        (6, 0),
        (7, 0),
        (13, 0), // H.264, ITU-T H.264 profiles.
        (14, 1), // VP8, RFC 6386.
        (19, 2),
        (20, 2),
        (21, 2),
        (22, 2), // VP9 profiles, VP9 bitstream spec.
        (32, 3),
        (33, 3),
        (39, 3), // AV1 profiles, AV1 bitstream spec.
    ];

    #[link(name = "dl")]
    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
    }

    struct Library(*mut c_void);
    impl Library {
        fn open(name: &CStr) -> Option<Self> {
            let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
            (!handle.is_null()).then_some(Self(handle))
        }
        fn symbol<T: Copy>(&self, name: &CStr) -> Option<T> {
            let address = unsafe { dlsym(self.0, name.as_ptr()) };
            if address.is_null() || std::mem::size_of::<T>() != std::mem::size_of_val(&address) {
                return None;
            }
            // SAFETY: the requested symbol is a C function with the matching
            // public libva ABI, and the pointer-sized function pointer is non-null.
            Some(unsafe { std::mem::transmute_copy(&address) })
        }
    }
    impl Drop for Library {
        fn drop(&mut self) {
            unsafe {
                dlclose(self.0);
            }
        }
    }

    type Display = *mut c_void;
    type GetDisplay = unsafe extern "C" fn(c_int) -> Display;
    type Initialize = unsafe extern "C" fn(Display, *mut c_int, *mut c_int) -> c_int;
    type Terminate = unsafe extern "C" fn(Display) -> c_int;
    type MaxProfiles = unsafe extern "C" fn(Display) -> c_int;
    type QueryProfiles = unsafe extern "C" fn(Display, *mut c_int, *mut c_int) -> c_int;
    type MaxEntrypoints = unsafe extern "C" fn(Display) -> c_int;
    type QueryEntrypoints = unsafe extern "C" fn(Display, c_int, *mut c_int, *mut c_int) -> c_int;
    type VendorString = unsafe extern "C" fn(Display) -> *const c_char;

    fn symbol<T: Copy>(lib: &Library, name: &'static [u8]) -> Option<T> {
        lib.symbol(CStr::from_bytes_with_nul(name).ok()?)
    }

    fn bounded_count(count: c_int) -> Option<usize> {
        let count = usize::try_from(count).ok()?;
        (count > 0 && count <= MAX_ITEMS).then_some(count)
    }

    fn with_node(path: &std::path::Path, drm: &Library, va: &Library) -> Option<CapabilityReport> {
        let fd = fs::OpenOptions::new().read(true).write(true).open(path).ok()?;
        let get_display: GetDisplay = symbol(drm, b"vaGetDisplayDRM\0")?;
        let display = unsafe { get_display(fd.as_raw_fd()) };
        if display.is_null() {
            return None;
        }
        let initialize: Initialize = symbol(va, b"vaInitialize\0")?;
        let terminate: Terminate = symbol(va, b"vaTerminate\0")?;
        let max_profiles: MaxProfiles = symbol(va, b"vaMaxNumProfiles\0")?;
        let query_profiles: QueryProfiles = symbol(va, b"vaQueryConfigProfiles\0")?;
        let max_entrypoints: MaxEntrypoints = symbol(va, b"vaMaxNumEntrypoints\0")?;
        let query_entrypoints: QueryEntrypoints = symbol(va, b"vaQueryConfigEntrypoints\0")?;
        let vendor_string: VendorString = symbol(va, b"vaQueryVendorString\0")?;
        let (mut major, mut minor) = (0, 0);
        if unsafe { initialize(display, &mut major, &mut minor) } != SUCCESS {
            return None;
        }

        let result = (|| {
            let p_count = bounded_count(unsafe { max_profiles(display) })?;
            let e_count = bounded_count(unsafe { max_entrypoints(display) })?;
            let mut profiles = vec![0; p_count];
            let mut actual_profiles = 0;
            if unsafe { query_profiles(display, profiles.as_mut_ptr(), &mut actual_profiles) } != SUCCESS
                || actual_profiles < 0
                || actual_profiles as usize > profiles.len()
            {
                return None;
            }
            let profiles = &profiles[..actual_profiles as usize];
            let mut codecs = Codecs::default();
            for &(profile, codec) in PROFILES {
                if !profiles.contains(&profile) {
                    continue;
                }
                let mut entries = vec![0; e_count];
                let mut actual_entries = 0;
                if unsafe { query_entrypoints(display, profile, entries.as_mut_ptr(), &mut actual_entries) } != SUCCESS
                    || actual_entries < 0
                    || actual_entries as usize > entries.len()
                {
                    continue;
                }
                if entries[..actual_entries as usize].contains(&VLD) {
                    match codec {
                        0 => codecs.h264 = true,
                        1 => codecs.vp8 = true,
                        2 => codecs.vp9 = true,
                        3 => codecs.av1 = true,
                        _ => {}
                    }
                }
            }
            let vendor_ptr = unsafe { vendor_string(display) };
            let vendor = if vendor_ptr.is_null() {
                None
            } else {
                // VA owns this NUL-terminated string until termination.
                unsafe { CStr::from_ptr(vendor_ptr) }.to_str().ok().map(str::to_owned)
            };
            Some(CapabilityReport { codecs, vendor })
        })();
        unsafe {
            terminate(display);
        }
        result
    }

    pub(super) fn probe() -> CapabilityReport {
        let Ok(va_name) = CString::new("libva.so.2") else {
            return CapabilityReport::default();
        };
        let Some(va) = Library::open(&va_name) else {
            return CapabilityReport::default();
        };
        let Ok(drm_name) = CString::new("libva-drm.so.2") else {
            return CapabilityReport::default();
        };
        let Some(drm) = Library::open(&drm_name) else {
            return CapabilityReport::default();
        };
        let Ok(nodes) = fs::read_dir("/dev/dri") else {
            return CapabilityReport::default();
        };
        let mut entries: Vec<_> = nodes.filter_map(Result::ok).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            let bytes = name.as_bytes();
            if !bytes.starts_with(b"renderD") || bytes.len() <= 7 || !bytes[7..].iter().all(u8::is_ascii_digit) {
                continue;
            }
            if let Some(report) = with_node(&entry.path(), &drm, &va) {
                return report;
            }
        }
        CapabilityReport::default()
    }
}

#[cfg(test)]
mod tests {
    use super::Codecs;

    #[test]
    fn capability_report_starts_unavailable() {
        assert_eq!(
            Codecs::default(),
            Codecs {
                h264: false,
                vp8: false,
                vp9: false,
                av1: false
            }
        );
        assert!(super::probe().vendor.is_none() || !super::probe().vendor.as_deref().unwrap_or_default().is_empty());
    }
}
