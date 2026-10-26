//! WinINet ABI declarations kept local to avoid expanding windows-sys features.
//! See Microsoft INTERNET_PER_CONN_OPTIONW and setting/retrieving Internet options.

use super::{SystemProxyBackend, SystemProxyError, SystemProxySnapshot, MAX_SETTING_UNITS};
use std::{ffi::c_void, mem::size_of, ptr};

const PER_CONNECTION_OPTION: u32 = 75;
const SETTINGS_CHANGED: u32 = 39;
const REFRESH: u32 = 37;
const FLAGS: u32 = 1;
const SERVER: u32 = 2;
const BYPASS: u32 = 3;
const PAC_URL: u32 = 4;
const FLAGS_UI: u32 = 10;

#[repr(C)]
#[derive(Clone, Copy)]
union OptionValue {
    number: u32,
    string: *mut u16,
    filetime: [u32; 2],
}

#[repr(C)]
struct PerConnectionOption {
    option: u32,
    value: OptionValue,
}

#[repr(C)]
struct PerConnectionOptions {
    size: u32,
    connection: *mut u16,
    count: u32,
    option_error: u32,
    options: *mut PerConnectionOption,
}

#[link(name = "wininet")]
extern "system" {
    fn InternetQueryOptionW(
        handle: *mut c_void,
        option: u32,
        buffer: *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn InternetSetOptionW(handle: *mut c_void, option: u32, buffer: *mut c_void, size: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
    fn GlobalFree(memory: *mut c_void) -> *mut c_void;
    fn GlobalSize(memory: *mut c_void) -> usize;
}

pub struct WindowsSystemProxyBackend;

impl SystemProxyBackend for WindowsSystemProxyBackend {
    fn snapshot(&self) -> Result<SystemProxySnapshot, SystemProxyError> {
        // FLAGS_UI preserves the user's autodetection checkbox on Windows 7+.
        // Microsoft explicitly recommends FLAGS as the compatibility fallback.
        query(FLAGS_UI).or_else(|_| query(FLAGS))
    }

    fn write_if_unchanged(
        &self,
        expected: &SystemProxySnapshot,
        desired: &SystemProxySnapshot,
    ) -> Result<(), SystemProxyError> {
        expected.validate()?;
        desired.validate()?;
        if self.snapshot()? != *expected {
            return Err(SystemProxyError::ExternalChange);
        }
        let mut server = wide(&desired.server);
        let mut bypass = wide(&desired.bypass);
        let mut pac_url = wide(&desired.pac_url);
        let mut options = [
            PerConnectionOption {
                option: FLAGS,
                value: OptionValue {
                    number: desired.flags,
                },
            },
            PerConnectionOption {
                option: SERVER,
                value: OptionValue {
                    string: server.as_mut_ptr(),
                },
            },
            PerConnectionOption {
                option: BYPASS,
                value: OptionValue {
                    string: bypass.as_mut_ptr(),
                },
            },
            PerConnectionOption {
                option: PAC_URL,
                value: OptionValue {
                    string: pac_url.as_mut_ptr(),
                },
            },
        ];
        let mut list = list(&mut options);
        // Null HINTERNET and null connection select the actual default LAN settings.
        // All pointers remain alive for the synchronous WinINet call.
        unsafe {
            if InternetSetOptionW(
                ptr::null_mut(),
                PER_CONNECTION_OPTION,
                (&mut list as *mut PerConnectionOptions).cast(),
                list.size,
            ) == 0
            {
                return Err(os_error("set"));
            }
            if InternetSetOptionW(ptr::null_mut(), SETTINGS_CHANGED, ptr::null_mut(), 0) == 0 {
                return Err(os_error("notify"));
            }
            if InternetSetOptionW(ptr::null_mut(), REFRESH, ptr::null_mut(), 0) == 0 {
                return Err(os_error("refresh"));
            }
        }
        Ok(())
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn list(options: &mut [PerConnectionOption; 4]) -> PerConnectionOptions {
    PerConnectionOptions {
        size: size_of::<PerConnectionOptions>() as u32,
        connection: ptr::null_mut(),
        count: options.len() as u32,
        option_error: 0,
        options: options.as_mut_ptr(),
    }
}

/// Owns strings allocated by InternetQueryOptionW even on partially failed queries.
struct QueryOptions([PerConnectionOption; 4]);

impl Drop for QueryOptions {
    fn drop(&mut self) {
        for option in &self.0[1..] {
            // Only string variants occupy these entries. They were zeroed before query.
            unsafe {
                let allocation = option.value.string;
                if !allocation.is_null() {
                    GlobalFree(allocation.cast());
                }
            }
        }
    }
}

fn query(flag_option: u32) -> Result<SystemProxySnapshot, SystemProxyError> {
    let mut owner = QueryOptions([
        PerConnectionOption {
            option: flag_option,
            value: OptionValue { number: 0 },
        },
        PerConnectionOption {
            option: SERVER,
            value: OptionValue {
                string: ptr::null_mut(),
            },
        },
        PerConnectionOption {
            option: BYPASS,
            value: OptionValue {
                string: ptr::null_mut(),
            },
        },
        PerConnectionOption {
            option: PAC_URL,
            value: OptionValue {
                string: ptr::null_mut(),
            },
        },
    ]);
    let mut list = list(&mut owner.0);
    let mut size = list.size;
    // The list and option array are valid writable buffers for the entire call.
    unsafe {
        if InternetQueryOptionW(
            ptr::null_mut(),
            PER_CONNECTION_OPTION,
            (&mut list as *mut PerConnectionOptions).cast(),
            &mut size,
        ) == 0
        {
            return Err(os_error("query"));
        }
        let snapshot = SystemProxySnapshot {
            flags: owner.0[0].value.number,
            server: read_string(owner.0[1].value.string)?,
            bypass: read_string(owner.0[2].value.string)?,
            pac_url: read_string(owner.0[3].value.string)?,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}

/// Bound reads by the allocation size and a hard limit, rather than unbounded C strlen.
/// Only called for WinINet strings that Microsoft specifies are GlobalFree allocations.
unsafe fn read_string(value: *mut u16) -> Result<String, SystemProxyError> {
    if value.is_null() {
        return Ok(String::new());
    }
    let bytes = GlobalSize(value.cast());
    if bytes == 0 || bytes > (MAX_SETTING_UNITS + 1) * 2 || bytes % 2 != 0 {
        return Err(SystemProxyError::InvalidSnapshot);
    }
    // Do not form a slice over the entire GlobalAlloc capacity: padding after the
    // documented null terminator need not have been initialized by WinINet.
    let mut units = Vec::new();
    for index in 0..bytes / 2 {
        let unit = value.add(index).read();
        if unit == 0 {
            return String::from_utf16(&units).map_err(|_| SystemProxyError::InvalidSnapshot);
        }
        if units.len() == MAX_SETTING_UNITS {
            return Err(SystemProxyError::InvalidSnapshot);
        }
        units.push(unit);
    }
    Err(SystemProxyError::InvalidSnapshot)
}

unsafe fn os_error(operation: &'static str) -> SystemProxyError {
    SystemProxyError::Windows {
        operation,
        code: GetLastError(),
    }
}

#[cfg(test)]
mod abi_tests {
    use super::*;

    #[test]
    fn native_option_structures_match_windows_abi() {
        if size_of::<usize>() == 8 {
            assert_eq!(size_of::<PerConnectionOption>(), 16);
            assert_eq!(size_of::<PerConnectionOptions>(), 32);
        } else {
            assert_eq!(size_of::<PerConnectionOption>(), 12);
            assert_eq!(size_of::<PerConnectionOptions>(), 20);
        }
    }
}
