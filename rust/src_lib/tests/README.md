# Wine Rust Library Tests

This directory contains tests for the Wine Rust library components.

## Test Organization

- `wine_http_tests.rs` - Tests for the HTTP module functionality
- `mod.rs` - Main test module that includes all test submodules

## Running Tests

The Wine Rust library uses the standard Rust testing framework. All tests can be run using `cargo test`.

### Running all tests

```bash
cargo test
```

### Running specific test modules

To run only the HTTP tests:

```bash
cargo test wine_http_tests
```

### Running specific test functions

To run a specific test function:

```bash
cargo test test_http_methods
```

### Test output options

- **Verbose output**: `cargo test -- --nocapture`
- **Show ignored tests**: `cargo test -- --include-ignored`
- **Filter test names**: `cargo test test_name`

## Test Structure

The tests are organized as follows:

- **Unit tests**: Tests that verify individual functions and components
- **Integration tests**: Tests that verify how components work together
- **Stub tests**: Tests that verify error handling for not-yet-implemented functionality

## Writing New Tests

When adding new functionality to the Wine Rust library:

1. Create a new test file in this directory (e.g., `wine_new_feature_tests.rs`)
2. Add the test module to `mod.rs`
3. Write comprehensive tests covering:
   - Normal operation
   - Error cases
   - Edge cases
   - Integration with other components

## Test Conventions

- Test functions should be named `test_*` to describe what they test
- Use descriptive assert messages
- Test both success and failure cases
- Keep tests focused on one specific behavior
- Use `#[test]` attribute for all test functions

## Current Test Coverage

The current tests cover:

- HTTP method constants
- HTTP status codes
- HTTP utility functions
- Error handling
- HTTP response/request creation
- HTTP Server API stubs
- HTTP Client API stubs
- HTTP constants and headers

## Troubleshooting

If tests fail:

1. Check that all dependencies are built
2. Verify that the Wine libraries are available
3. Ensure the bindings are up to date
4. Check for any missing FFI functions

## Continuous Integration

The tests are designed to run in CI environments. Make sure all tests pass before submitting changes.