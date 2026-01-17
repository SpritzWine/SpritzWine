use std::ffi::{CStr, c_char};

use crate::wine_debug;

pub static TEST: &CStr = c"Hi mom!";

#[unsafe(no_mangle)]
pub extern "C" fn hello_from_rs() -> *const c_char {
    wine_debug::wine_error("some test error").unwrap();
    c"Hello C!".as_ptr()
}
