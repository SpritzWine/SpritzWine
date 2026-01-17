// Simple test that doesn't require FFI bindings

use wine_api::{wine_core, wine_debug};

fn main() {
    println!("=== Testing Wine Rust API (Simple Mode) ===");

    // Test hello_world
    wine_debug::hello_world();

    // Test all debug logging functions
    wine_debug::wine_trace("This is a trace message");
    wine_debug::wine_warn("This is a warning message");
    wine_debug::wine_error("This is an error message");
    wine_debug::wine_fixme("This is a fixme message");

    // Test string functions
    let test_string = "Hello, Wine from Rust!";
    let dbg_a = wine_core::strings::wine_dbgstr_a(test_string);
    let dbg_w = wine_core::strings::wine_dbgstr_w(test_string);

    println!("ANSI string: {}", dbg_a);
    println!("Unicode string: {}", dbg_w);

    // Test module functions
    let module = wine_core::module::get_current_module();
    println!("Current module: {:?}", module);

    // Test error functions
    let last_error = wine_core::error::get_last_error();
    println!("Last error: {}", last_error);

    wine_core::error::set_last_error(42);
    let new_error = wine_core::error::get_last_error();
    println!("New error: {}", new_error);

    println!("=== Test completed successfully! ===");
}
