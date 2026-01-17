//! Example Wine DLL implemented in Rust
//!
//! This demonstrates how to create a Wine-compatible DLL using Rust.
//! The DLL exports standard Windows DLL functions and can be loaded by Wine.

use std::ffi::c_void;

// Import Wine types and functions
use libwine::wine_debug::{wine_trace, wine_warn, wine_error};
use libwine::wine_core::module::get_current_module;

// Windows DLL entry point constants (from winbase.h)
const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_PROCESS_DETACH: u32 = 0;
const DLL_THREAD_ATTACH: u32 = 2;
const DLL_THREAD_DETACH: u32 = 3;

/// DLL entry point - equivalent to DllMain in C
#[no_mangle]
pub extern "system" fn DllMain(
    hmodule: *mut c_void,
    reason: u32,
    _reserved: *mut c_void,
) -> bool {
    match reason {
        DLL_PROCESS_ATTACH => {
            wine_trace("Rust DLL: Process attach");
            wine_trace(&format!("Module handle: {:?}", hmodule));
            
            // Initialize any global state here
            true
        }
        DLL_PROCESS_DETACH => {
            wine_trace("Rust DLL: Process detach");
            
            // Clean up any global state here
            true
        }
        DLL_THREAD_ATTACH => {
            wine_trace("Rust DLL: Thread attach");
            true
        }
        DLL_THREAD_DETACH => {
            wine_trace("Rust DLL: Thread detach");
            true
        }
        _ => {
            wine_warn(&format!("Rust DLL: Unknown reason code {}", reason));
            true
        }
    }
}

/// Example exported function that demonstrates Rust-Wine integration
#[no_mangle]
pub extern "system" fn rust_sample_function(input: i32) -> i32 {
    wine_trace(&format!("rust_sample_function called with input: {}", input));
    
    // Demonstrate using Wine debug functions
    if input < 0 {
        wine_warn("Negative input value detected");
    }
    
    // Simple processing
    let result = input * 2;
    wine_trace(&format!("Returning result: {}", result));
    
    result
}

/// Example function that demonstrates string handling
#[no_mangle]
pub extern "system" fn rust_string_function(input: *const i8) -> *const i8 {
    use std::ffi::CStr;
    
    wine_trace("rust_string_function called");
    
    if input.is_null() {
        wine_error("Null pointer received");
        return std::ptr::null();
    }
    
    // Convert C string to Rust string
    let c_str = unsafe { CStr::from_ptr(input) };
    let rust_str = match c_str.to_str() {
        Ok(s) => s,
        Err(e) => {
            wine_error(&format!("Invalid UTF-8 string: {}", e));
            return std::ptr::null();
        }
    };
    
    wine_trace(&format!("Received string: {}", rust_str));
    
    // Process the string (simple uppercase conversion)
    let processed = rust_str.to_uppercase();
    
    // Convert back to C string
    // Note: In a real implementation, we'd need proper memory management
    // For this example, we'll just return the input as a placeholder
    input
}

/// Example function that demonstrates error handling
#[no_mangle]
pub extern "system" fn rust_error_demo(should_fail: bool) -> bool {
    wine_trace(&format!("rust_error_demo called with should_fail: {}", should_fail));
    
    if should_fail {
        wine_error("Intentionally failing as requested");
        false
    } else {
        wine_trace("Operation completed successfully");
        true
    }
}

// Add more exported functions as needed...

// Helper functions for internal use
fn init_dll() {
    wine_trace("Initializing Rust DLL");
    // Perform any initialization here
}

fn cleanup_dll() {
    wine_trace("Cleaning up Rust DLL");
    // Perform any cleanup here
}