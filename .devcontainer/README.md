# Dev Container Setup

This directory contains the configuration for the Wine Rust development container.

## What's Included

- **Fedora 44 base image** with all necessary development tools
- **Rust toolchain** with cargo, rustc, and common tools
- **LLVM/Clang** for FFI bindings generation
- **Wine dependencies** for building Wine components
- **VS Code extensions** for Rust and C++ development

## Key Features

### 1. Pre-installed Dependencies

The container includes:
- **LLVM and libclang** for bindgen FFI generation
- **Rust toolchain** with cargo, rustc, clippy, rustfmt
- **Wine build dependencies** (X11, OpenSSL, etc.)
- **Development tools** (gcc, make, git, etc.)

### 2. Automatic LIBCLANG_PATH Configuration

The container automatically sets:
```bash
LIBCLANG_PATH=/usr/lib64/llvm/lib
```

This ensures bindgen can find libclang without manual configuration.

### 3. VS Code Extensions

- **Rust Analyzer** - Advanced Rust language support
- **C/C++** - IntelliSense for C/C++ code
- **CMake Tools** - CMake project support

### 4. Post-Creation Setup

When the container starts, it runs:
```bash
./configure --without-freetype
```

This configures the Wine build environment.

## Usage

### Building the Container

```bash
# Build the dev container
devcontainer build

# Or open in VS Code with Remote-Containers extension
# Press F1 > "Remote-Containers: Reopen in Container"
```

### Building Wine Rust Components

```bash
# Navigate to rust directory
cd /workspace/rust

# Build the project
cargo build

# Run tests
cargo test
```

### Common Commands

```bash
# Update dependencies
cargo update

# Check code formatting
cargo fmt --check

# Run clippy
cargo clippy

# Build with verbose output
cargo build -v
```

## Troubleshooting

### "libclang not found" Errors

The container should automatically find libclang, but if you see errors:

1. **Verify libclang is installed**:
   ```bash
   ls /usr/lib64/llvm/lib/libclang.so*
   ```

2. **Check LIBCLANG_PATH**:
   ```bash
   echo $LIBCLANG_PATH
   ```

3. **Manually set the path** (if needed):
   ```bash
   export LIBCLANG_PATH=/usr/lib64/llvm/lib
   ```

### Rust Toolchain Issues

```bash
# Update rust toolchain
rustup update

# Check versions
rustc --version
cargo --version
```

## Customization

### Adding More Dependencies

Edit `Dockerfile` and add packages to the `dnf install` command, then rebuild:

```bash
devcontainer rebuild
```

### Changing Rust Version

Modify the Rust installation in `Dockerfile`:

```bash
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
```

## Development Workflow

1. **Open project in container**: Use VS Code Remote-Containers
2. **Build and test**: `cargo build` and `cargo test`
3. **Format code**: `cargo fmt`
4. **Check linting**: `cargo clippy`
5. **Update dependencies**: `cargo update`

## Notes

- The container uses **Fedora 44** as the base image
- **Rust toolchain** is installed via rustup
- **LIBCLANG_PATH** is pre-configured for bindgen
- **Wine dependencies** are included for full compatibility

For more information about dev containers, see the [official documentation](https://code.visualstudio.com/docs/remote/containers).