//! Wine debug functionality
//!
//! This module provides safe Rust bindings to Wine's debug system.
//! It uses the proper Wine debug functions for authentic integration.

use std::ffi::{CString, c_char};
use std::os::raw::c_int;

/// Wine debug channel structure
#[repr(C)]
pub struct WineDebugChannel {
    flags: u8,
    name: *const c_char,
}

// Safety: WineDebugChannel contains a raw pointer but it's only used for FFI
// and points to a static string, so it's safe to send and sync
unsafe impl Send for WineDebugChannel {}
unsafe impl Sync for WineDebugChannel {}

/// Default debug channel for our module
static DEFAULT_CHANNEL: WineDebugChannel = WineDebugChannel {
    flags: 0, // Will be initialized by Wine
    name: c"wine_rust".as_ptr(),
};

/// Safe wrapper around Wine's debug header function
fn wine_debug_header(
    cls: crate::bindings::__wine_debug_class,
    channel: *mut crate::bindings::__wine_debug_channel,
    function: &str,
) -> Result<i32, String> {
    let c_function =
        CString::new(function).map_err(|e| format!("Failed to convert function name: {}", e))?;

    let result = unsafe { crate::bindings::__wine_dbg_header(cls, channel, c_function.as_ptr()) };

    Ok(result)
}

/// Safe wrapper around Wine's debug output function
fn wine_debug_output(message: &str) -> Result<c_int, String> {
    let c_string = CString::new(message).map_err(|e| format!("Failed to convert string: {}", e))?;

    let result = unsafe { crate::bindings::__wine_dbg_output(c_string.as_ptr()) };

    Ok(result)
}

/// Write a formatted debug message using Wine's proper debug system
fn wine_debug_log(
    cls: crate::bindings::__wine_debug_class,
    channel: &WineDebugChannel,
    function: &str,
    message: &str,
) -> Result<(), String> {
    // Convert our WineDebugChannel to the bindings type
    let bindgen_channel = crate::bindings::__wine_debug_channel {
        flags: channel.flags,
        name: {
            let mut name_array = [0i8; 15];
            let name_bytes = unsafe { std::ffi::CStr::from_ptr(channel.name).to_bytes() };
            let copy_len = std::cmp::min(name_bytes.len(), 14); // Leave room for null terminator
            for i in 0..copy_len {
                name_array[i] = name_bytes[i] as i8;
            }
            name_array[copy_len] = 0; // Null terminator
            name_array
        },
    };

    // Try to write the header first
    let header_result = wine_debug_header(cls, &bindgen_channel as *const _ as *mut _, function);

    // If header was written (result >= 0), write the message
    if header_result.unwrap_or(-1) >= 0 {
        wine_debug_output(message)?;
    }

    Ok(())
}

/// Safe wrapper for Wine trace messages
pub fn wine_trace(message: &str) -> Result<(), String> {
    wine_debug_log(
        crate::bindings::__wine_debug_class::__WINE_DBCL_TRACE,
        &DEFAULT_CHANNEL,
        "wine_trace",
        message,
    )
}

/// Safe wrapper for Wine warnings
pub fn wine_warn(message: &str) -> Result<(), String> {
    wine_debug_log(
        crate::bindings::__wine_debug_class::__WINE_DBCL_WARN,
        &DEFAULT_CHANNEL,
        "wine_warn",
        message,
    )
}

/// Safe wrapper for Wine errors
pub fn wine_error(message: &str) -> Result<(), String> {
    wine_debug_log(
        crate::bindings::__wine_debug_class::__WINE_DBCL_ERR,
        &DEFAULT_CHANNEL,
        "wine_error",
        message,
    )
}

/// Safe wrapper for Wine fixme messages
pub fn wine_fixme(message: &str) -> Result<(), String> {
    wine_debug_log(
        crate::bindings::__wine_debug_class::__WINE_DBCL_FIXME,
        &DEFAULT_CHANNEL,
        "wine_fixme",
        message,
    )
}

/// Macro for convenient debug logging
#[macro_export]
macro_rules! wine_trace {
    ($($arg:tt)*) => {
        if let Err(e) = $crate::wine_debug::wine_trace(&format!($($arg)*)) {
            eprintln!("Debug output failed: {}", e);
        }
    };
}

/// Macro for convenient warning logging
#[macro_export]
macro_rules! wine_warn {
    ($($arg:tt)*) => {
        if let Err(e) = $crate::wine_debug::wine_warn(&format!($($arg)*)) {
            eprintln!("Debug output failed: {}", e);
        }
    };
}

/// Macro for convenient error logging
#[macro_export]
macro_rules! wine_error {
    ($($arg:tt)*) => {
        if let Err(e) = $crate::wine_debug::wine_error(&format!($($arg)*)) {
            eprintln!("Debug output failed: {}", e);
        }
    };
}

/// Macro for convenient fixme logging
#[macro_export]
macro_rules! wine_fixme {
    ($($arg:tt)*) => {
        if let Err(e) = $crate::wine_debug::wine_fixme(&format!($($arg)*)) {
            eprintln!("Debug output failed: {}", e);
        }
    };
}

pub fn hello_world() {
    wine_trace!("Hello, world from Wine Rust bindings!");
}
