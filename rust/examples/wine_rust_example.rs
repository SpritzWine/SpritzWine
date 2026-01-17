// Example program demonstrating Wine Rust bindings usage
//
// This example shows how to use the Wine debug functionality from Rust
// to integrate properly with Wine's debugging system.

use wine_api::wine_debug;
use wine_api::{wine_error, wine_fixme, wine_trace, wine_warn};

fn main() {
    println!("Wine Rust Bindings Example");
    println!("==========================");

    // Test different debug levels
    wine_debug::wine_trace("This is a trace message").expect("Trace failed");
    wine_debug::wine_warn("This is a warning message").expect("Warn failed");
    wine_debug::wine_error("This is an error message").expect("Error failed");
    wine_debug::wine_fixme("This is a fixme message").expect("Fixme failed");

    // Test the macro versions
    wine_trace!("Macro trace: Application started");
    wine_warn!("Macro warn: Potential issue detected");
    wine_error!("Macro error: Something went wrong!");
    wine_fixme!("Macro fixme: This feature needs implementation");

    // Test with formatted strings
    let version = "1.0.0";
    let feature = "debug_output";
    wine_trace!("Wine Rust Bindings v{} - Testing {}", version, feature);

    // Test error handling
    match wine_debug::wine_trace("This should work") {
        Ok(_) => wine_trace!("Successfully called Wine debug function"),
        Err(e) => wine_error!("Failed to call Wine debug function: {}", e),
    }

    println!("Example completed successfully!");
}
