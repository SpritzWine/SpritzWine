pub mod rswine;

pub mod wine_core;
pub mod wine_debug;
pub mod wine_http;

// Include the generated bindings
#[allow(dead_code)]
#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[allow(unnecessary_transmutes)]
#[allow(unsafe_op_in_unsafe_fn)]
mod bindings {
    include!("../generated/bindings.rs");
}
// Re-export the debug-related items from bindings
#[allow(unused_imports)]
pub use bindings::*;

// Include manual bindings for types that bindgen cannot handle
mod manual_bindings;
// Re-export manual bindings
pub use manual_bindings::*;
