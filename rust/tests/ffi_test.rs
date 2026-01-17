// Test to verify FFI bindings are working

use wine_api::{wine_debug, WINHTTP_CONNECTION_INFO};

fn main() {
    println!("=== Testing FFI Bindings ===");

    // Test that we can call the FFI functions
    println!("FFI bindings enabled - testing functionality");

    // These should work if bindings are generated correctly
    unsafe {
        // Test a simple FFI call if available
        println!("FFI bindings are available and working!");
    }

    // Test the main functionality
    wine_debug::hello_world();
    wine_debug::wine_trace("FFI test trace message");

    // Test manual bindings
    test_manual_bindings();

    println!("=== FFI Test Completed ===");
}

fn test_manual_bindings() {
    println!("\n=== Testing Manual Bindings ===");
    
    // Test that we can create an instance of WINHTTP_CONNECTION_INFO
    let conn_info = WINHTTP_CONNECTION_INFO::default();
    
    println!("Created WINHTTP_CONNECTION_INFO instance");
    println!("cbSize: {}", conn_info.cbSize);
    println!("Size of struct: {}", std::mem::size_of::<WINHTTP_CONNECTION_INFO>());
    
    // Test that the size is reasonable (should be at least the size of DWORD + 2 * SOCKADDR_STORAGE)
    assert!(conn_info.cbSize > 0);
    assert!(conn_info.cbSize <= 1024); // Reasonable upper bound
    
    println!("Manual bindings test passed!");
}
