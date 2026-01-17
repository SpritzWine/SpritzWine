use wine_api::wine_debug;
use wine_api::{wine_error, wine_fixme, wine_trace, wine_warn};

fn main() {
    // Demonstrate Wine debug logging using the new proper API
    wine_trace!("Rust Wine integration demo started");

    // Test all debug levels
    wine_debug::wine_trace("This is a trace message from Rust")
        .expect("Failed to output trace message");
    wine_debug::wine_warn("This is a warning message from Rust")
        .expect("Failed to output warning message");
    wine_debug::wine_error("This is an error message from Rust")
        .expect("Failed to output error message");
    wine_debug::wine_fixme("This is a fixme message from Rust - functionality not yet implemented")
        .expect("Failed to output fixme message");

    // Test macro versions with formatting
    let version = "1.0.0";
    let platform = "Wine";
    wine_trace!("Rust {} integration with {} - Demo", version, platform);
    wine_warn!("This is a formatted warning: {}", "test condition");
    wine_error!("Formatted error: {}", "simulated error");
    wine_fixme!("Need to implement: {}", "advanced features");

    // Demonstrate error handling
    match wine_debug::wine_trace("Final test message") {
        Ok(_) => wine_trace!("Successfully completed all debug operations"),
        Err(e) => wine_error!("Debug operation failed: {}", e),
    }

    unsafe {
        wine_api::__wine_dbg_output(c"test".as_ptr());
    }
    println!("Rust Wine integration demo completed successfully!");
}
