# Wine Rust API Tests

This directory contains tests for the Wine Rust API implementation.

## Test Structure

```
tests/
├── README.md               # This file
├── simple_test.rs          # Simple test without FFI dependencies
├── test_minimal.rs         # Minimal functionality test
└── test_poc.rs             # Proof-of-concept test
```

## Test Types

### 1. Simple Tests (No FFI Required)
- **`simple_test.rs`**: Tests basic functionality without FFI bindings
- **`test_minimal.rs`**: Minimal test for core functionality
- **Run with**: `cargo run --bin simple_test`

### 2. POC Test (Demonstration)
- **`test_poc.rs`**: Demonstrates the full API usage
- **Run with**: `cargo run --bin test_poc`

## Running Tests

### Basic Tests (No FFI)
```bash
cd /media/SSD/Projekte/wine/rust
cargo run --bin simple_test
```

### With FFI Bindings
```bash
cd /media/SSD/Projekte/wine/rust
cargo build --features wine-ffi
cargo run --bin test_poc --features wine-ffi
```

## Test Organization

### Simple Test (`simple_test.rs`)
Demonstrates basic functionality without requiring FFI bindings:
- Debug logging functions
- String conversion functions
- Module and error functions

### Minimal Test (`test_minimal.rs`)
Tests the most basic functionality:
- Hello world
- Basic logging
- String handling

### POC Test (`test_poc.rs`)
Comprehensive test demonstrating:
- All debug logging levels
- String conversion (ANSI/Unicode)
- Error handling
- Module management

## Adding New Tests

To add a new test:

1. **Create a new test file**:
   ```bash
   touch tests/my_new_test.rs
   ```

2. **Add test code**:
   ```rust
   mod wine_debug;
   mod wine_core;
   
   fn main() {
       // Your test code here
       wine_debug::hello_world();
       // ...
   }
   ```

3. **Add to Cargo.toml**:
   ```toml
   [[bin]]
   name = "my_new_test"
   path = "tests/my_new_test.rs"
   ```

4. **Run the test**:
   ```bash
   cargo run --bin my_new_test
   ```

## Test Development Guidelines

1. **Keep tests simple**: Each test should focus on one aspect
2. **Use descriptive names**: Test files should clearly indicate what they test
3. **Document tests**: Add comments explaining what each test demonstrates
4. **Handle errors gracefully**: Tests should fail clearly if something is wrong
5. **Test both modes**: Ensure tests work with and without FFI bindings

## Future Test Enhancements

- Add unit tests with `#[test]` attributes
- Create integration tests for Wine API interaction
- Add benchmark tests for performance
- Implement property-based testing
- Add CI/CD test automation