# Wine Rust Examples

This directory contains example Rust modules that demonstrate integration with Wine.

## Sample DLL

The `sample_dll` directory contains a complete example of a Wine-compatible DLL implemented in Rust.

### Features Demonstrated:

- **DLL Entry Point**: Implementation of `DllMain` equivalent in Rust
- **FFI Integration**: Safe wrappers around Wine C APIs
- **Debug Logging**: Using Wine's debug logging system from Rust
- **String Handling**: Converting between Rust and C strings
- **Error Handling**: Proper error handling patterns

### Building the Sample DLL

```bash
cd rust/examples/sample_dll
cargo build --release
```

This will generate a `.dll` file that can be loaded by Wine.

### Using the Sample DLL

The DLL exports several functions that can be called from C code:

- `rust_sample_function(input: i32) -> i32` - Simple integer processing
- `rust_string_function(input: *const i8) -> *const i8` - String processing
- `rust_error_demo(should_fail: bool) -> bool` - Error handling demonstration

### Example Usage from C

```c
// Load the DLL
HMODULE hModule = LoadLibrary("sample_dll.dll");

// Get function pointers
typedef int (*RustSampleFunction)(int);
RustSampleFunction sampleFunc = (RustSampleFunction)GetProcAddress(hModule, "rust_sample_function");

// Call the function
int result = sampleFunc(42);
printf("Result: %d\n", result);
```

## Creating Your Own Wine Components in Rust

### Basic Structure

1. **Cargo.toml**: Configure as a `cdylib` crate type
2. **lib.rs**: Implement your DLL functions with `#[no_mangle]` and `extern "system"`
3. **FFI**: Use `libwine` crate for safe Wine API access

### Key Patterns

#### DLL Entry Point

```rust
#[no_mangle]
pub extern "system" fn DllMain(
    hmodule: *mut c_void,
    reason: u32,
    _reserved: *mut c_void,
) -> bool {
    match reason {
        DLL_PROCESS_ATTACH => {
            // Initialize your DLL
            true
        }
        DLL_PROCESS_DETACH => {
            // Clean up your DLL
            true
        }
        // Handle other cases...
    }
}
```

#### Exporting Functions

```rust
#[no_mangle]
pub extern "system" fn my_exported_function(param: i32) -> i32 {
    // Your implementation here
    param * 2
}
```

#### Using Wine Debug Logging

```rust
use libwine::wine_debug::{wine_trace, wine_warn, wine_error};

wine_trace("This is a trace message");
wine_warn("This is a warning");
wine_error("This is an error");
```

## Future Examples

This directory will be expanded with more examples demonstrating:

- Wine driver implementation in Rust
- Wine subsystem integration
- Complex FFI patterns
- Memory management strategies
- Performance optimization techniques