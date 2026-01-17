// Simple test to verify Wine debug functionality
use wine_api::wine_debug;
use wine_api::{wine_error, wine_fixme, wine_trace, wine_warn};

#[test]
fn test_wine_debug_output() {
    // Test that we can call the Wine debug functions without panicking
    let result = wine_debug::wine_trace("Test trace message");
    assert!(result.is_ok(), "wine_trace should succeed");

    let result = wine_debug::wine_warn("Test warning message");
    assert!(result.is_ok(), "wine_warn should succeed");

    let result = wine_debug::wine_error("Test error message");
    assert!(result.is_ok(), "wine_error should succeed");

    let result = wine_debug::wine_fixme("Test fixme message");
    assert!(result.is_ok(), "wine_fixme should succeed");
}

#[test]
fn test_wine_debug_macros() {
    // Test that the macros work
    wine_trace!("Macro test: {}", "trace");
    wine_warn!("Macro test: {}", "warn");
    wine_error!("Macro test: {}", "error");
    wine_fixme!("Macro test: {}", "fixme");
}
