# Wine Rust Bindings

This project provides Rust bindings for Wine's C APIs, enabling Rust code to integrate seamlessly with the Wine Windows compatibility layer.

## Features

- **Wine Debug Integration**: Full access to Wine's debugging system (`TRACE`, `WARN`, `ERR`, `FIXME`)
- **Safe Rust API**: Type-safe wrappers around Wine's C APIs
- **Macro Support**: Convenient macros for common debugging operations
- **Proper Error Handling**: Rust-friendly error handling for FFI operations

## Prerequisites

- Rust toolchain (stable or nightly)
- Wine development environment
- libclang (for bindgen)
- 32-bit build tools (since Wine is 32-bit)

## Setup

### 1. Install Dependencies

```bash
# On Fedora/RHEL
sudo dnf install llvm libclang-devel gcc-multilib

# On Ubuntu/Debian
sudo apt-get install llvm clang libclang-dev gcc-multilib
```

### 2. Configure Rust for 32-bit Target

Create a `.cargo/config.toml` file in your project root:

```toml
[build]
target = "i686-unknown-linux-gnu"

[target.i686-unknown-linux-gnu]
linker = "gcc"
rustflags = ["-C", "link-arg=-m32"]
```

### 3. Build Wine

Ensure Wine is built with debug functions available:

```bash
# In the Wine source directory
./configure --enable-winedbg
make
```

## Usage

### Basic Example

```rust
use wine_api::wine_debug;

fn main() {
    // Use Wine's debug functions
    wine_debug::wine_trace("Application started").expect("Trace failed");
    wine_debug::wine_warn("Potential issue detected").expect("Warn failed");
    
    // Or use the convenient macros
    wine_trace!("This is a trace message");
    wine_warn!("This is a warning");
    wine_error!("This is an error");
    wine_fixme!("This needs implementation");
}
```

### Running the Example

```bash
# Build and run the example
cd examples
cargo run --example wine_rust_example
```

### Using in Your Project

Add this to your `Cargo.toml`:

```toml
[dependencies]
wine_api = { path = "../rust/src_lib" }
```

## API Documentation

### Debug Functions

#### `wine_trace(message: &str) -> Result<(), String>`

Output a trace-level debug message.

```rust
wine_debug::wine_trace("Entering function")?;
```

#### `wine_warn(message: &str) -> Result<(), String>`

Output a warning-level debug message.

```rust
wine_debug::wine_warn("Unexpected condition")?;
```

#### `wine_error(message: &str) -> Result<(), String>`

Output an error-level debug message.

```rust
wine_debug::wine_error("Operation failed")?;
```

#### `wine_fixme(message: &str) -> Result<(), String>`

Output a fixme-level debug message (for unimplemented features).

```rust
wine_debug::wine_fixme("Feature not yet implemented")?;
```

### Debug Macros

#### `wine_trace!(format, ...)`

Convenient macro for trace messages with formatting.

```rust
wine_trace!("Value: {}", value);
```

#### `wine_warn!(format, ...)`

Convenient macro for warning messages with formatting.

```rust
wine_warn!("Invalid value: {}", value);
```

#### `wine_error!(format, ...)`

Convenient macro for error messages with formatting.

```rust
wine_error!("Failed to open file: {}", path);
```

#### `wine_fixme!(format, ...)`

Convenient macro for fixme messages with formatting.

```rust
wine_fixme!("Need to implement {}", feature_name);
```

## Debugging

### Viewing Debug Output

Set the `WINEDEBUG` environment variable to control debug output:

```bash
# Show all debug messages
export WINEDEBUG=+all

# Show only trace and error messages
export WINEDEBUG=+trace,+err

# Show messages from specific channels
export WINEDEBUG=+relay,+wine_rust
```

### Common Issues

#### "Cannot find -lntdll"

Ensure you're using the correct library path. The build script should automatically find `ntdll.so`.

#### Architecture Mismatch

Make sure you're building for 32-bit target (`i686-unknown-linux-gnu`).

#### Missing Functions

If functions are not found, verify that:
1. Wine is built with the functions exported (check the `.spec` files)
2. The libraries are rebuilt after modifying `.spec` files
3. You're linking against the correct library (`.so` vs `.dll.so`)

## Build Script Details

The build script (`build.rs`) handles:

- **Library Detection**: Automatically finds Wine libraries
- **Binding Generation**: Uses bindgen to create Rust bindings from C headers
- **Cross-Platform Support**: Handles 32-bit/64-bit compatibility
- **Error Handling**: Provides clear error messages for common issues

## Contributing

Contributions are welcome! Please follow these guidelines:

1. **Test Changes**: Ensure your changes work with both Wine and Rust
2. **Documentation**: Update documentation for new features
3. **Error Handling**: Provide meaningful error messages
4. **Compatibility**: Maintain compatibility with Wine's API

## License

This project is licensed under the same terms as Wine (LGPL).

## Support

For issues or questions:

- Check the Wine documentation
- Review the Wine source code for API details
- Consult Rust's FFI documentation for binding patterns

## Example Output

When running the example with `WINEDEBUG=+all`, you should see output like:

```
trace:wine_rust:This is a trace message
trace:wine_rust:Macro trace: Application started
warn:wine_rust:This is a warning message
warn:wine_rust:Macro warn: Potential issue detected
err:wine_rust:This is an error message
err:wine_rust:Macro error: Something went wrong!
fixme:wine_rust:This is a fixme message
fixme:wine_rust:Macro fixme: This feature needs implementation
```

The debug output will be properly integrated with Wine's debugging system and can be filtered and controlled using standard Wine debug tools.