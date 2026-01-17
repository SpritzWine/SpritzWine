// Minimal test without dependencies
fn main() {
    println!("Testing Wine Rust API...");
    
    // Test the basic functionality
    println!("Hello, world!");
    println!("[TRACE] Test message");
    println!("[WARN] Test warning");
    println!("[ERROR] Test error");
    println!("[FIXME] Test fixme");
    
    // Test string functions
    let test_str = "Hello, Wine!";
    println!("String test: {:?}", test_str);
    
    println!("Test completed!");
}
