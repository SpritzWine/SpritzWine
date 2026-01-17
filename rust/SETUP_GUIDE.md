# Wine Rust Development Setup Guide

This guide helps you set up the Wine Rust development environment and resolve common issues.

## Prerequisites

### 1. Build Wine First

The Rust bindings depend on Wine's native libraries. You need to build Wine first:

```bash
cd /workspace
./configure
make -j$(nproc)
```

### 2. Install LLVM/Clang

The build script requires libclang for FFI bindings generation:

```bash
# On Fedora
sudo dnf install -y llvm libclang-devel clang

# On Ubuntu/Debian
sudo apt-get install -y llvm libclang-dev clang

# On macOS (Homebrew)
brew install llvm
```

## Common Issues and Solutions

### Issue: "could not find native static library `ntdll`"

**Cause**: The Rust build script can't find Wine's native libraries.

**Solutions**:

1. **Build Wine first** (recommended):
   ```bash
   cd /workspace
   ./configure
   make -j$(nproc)
   cd rust
   cargo build
   ```

2. **Check library paths**:
   ```bash
   ls -la ../../ntdll ../../kernel32 ../../user32
   ```

3. **Set library search path**:
   ```bash
   export LIBRARY_PATH=/workspace:$LIBRARY_PATH
   ```

### Issue: "Unable to find libclang"

**Cause**: bindgen cannot find libclang for FFI generation.

**Solutions**:

1. **Install LLVM**:
   ```bash
   sudo dnf install -y llvm libclang-devel
   ```

2. **Set LIBCLANG_PATH manually**:
   ```bash
   # Try these paths
   export LIBCLANG_PATH=/usr/lib64/llvm20/lib64  # LLVM 20
   export LIBCLANG_PATH=/usr/lib64/rocm/llvm/lib  # ROCm LLVM
   export LIBCLANG_PATH=/usr/lib64  # System path
   ```

3. **Verify libclang exists**:
   ```bash
   find /usr -name "libclang.so*" 2>/dev/null
   ```

### Issue: SELinux AVC Denials

**Cause**: SELinux is blocking file operations.

**Solutions**:

1. **Set SELinux to permissive mode** (temporary):
   ```bash
   sudo setenforce 0
   ```

2. **Label the workspace correctly**:
   ```bash
   sudo chcon -R -t container_file_t /workspace
   ```

3. **Add SELinux policy** (advanced):
   ```bash
   # See .devcontainer/README.md for policy creation
   ```

## Build Script Behavior

The build script now:

1. **Auto-detects libclang** in common locations
2. **Checks for Wine libraries** before linking
3. **Provides helpful warnings** when dependencies are missing
4. **Generates FFI bindings** when all dependencies are available

### Expected Build Output

```
cargo:warning=************************************************
cargo:warning=* Wine Rust FFI Build Script *
cargo:warning=* This script generates Rust bindings for Wine C APIs
cargo:warning=* Requires: libclang (for bindgen) and Wine libraries
cargo:warning=************************************************
cargo:warning=Found libclang at: /usr/lib64/llvm20/lib64
cargo:rustc-link-search=../../
cargo:warning=Wine library ntdll not found at ../../ntdll, skipping
cargo:warning=Wine library kernel32 not found at ../../kernel32, skipping
cargo:warning=Wine library user32 not found at ../../user32, skipping
```

## Development Workflow

### 1. Initial Setup

```bash
# Clone the repository
git clone https://github.com/wine-mirror/wine.git
cd wine

# Build Wine
./configure
make -j$(nproc)

# Install Rust dependencies
cd rust
cargo update
```

### 2. Building

```bash
# Build with default features
cargo build

# Build with verbose output
cargo build -v

# Run tests
cargo test
```

### 3. Common Commands

```bash
# Format code
cargo fmt

# Check linting
cargo clippy

# Update dependencies
cargo update

# Clean build
cargo clean && cargo build
```

## Dev Container Setup

The `.devcontainer/` directory contains a pre-configured development environment:

- **Dockerfile**: Installs all required dependencies
- **devcontainer.json**: VS Code configuration
- **LIBCLANG_PATH**: Pre-configured to `/usr/lib64/rocm/llvm/lib`

To use:
```bash
# Open in VS Code with Remote-Containers extension
# Press F1 > "Remote-Containers: Reopen in Container"
```

## Troubleshooting Checklist

1. ✅ **Wine built successfully** (`make` completed)
2. ✅ **LLVM/clang installed** (`libclang.so` exists)
3. ✅ **LIBCLANG_PATH set correctly** (points to valid libclang)
4. ✅ **Wine libraries exist** (`ntdll`, `kernel32`, `user32`)
5. ✅ **SELinux not blocking** (permissive mode or proper labeling)
6. ✅ **Build script can find dependencies** (check warning messages)

## Getting Help

If you're still having issues:

1. **Check build output** for specific error messages
2. **Verify file paths** with `ls` and `find`
3. **Test libclang detection**:
   ```bash
   find /usr -name "libclang.so*" 2>/dev/null
   ```
4. **Check Wine libraries**:
   ```bash
   ls -la ../../{ntdll,kernel32,user32}
   ```
5. **Review build script warnings** for guidance

## Additional Resources

- **Wine Documentation**: https://wiki.winehq.org/
- **Rust FFI Guide**: https://doc.rust-lang.org/nomicon/ffi.html
- **Bindgen Documentation**: https://rust-lang.github.io/rust-bindgen/
- **SELinux Guide**: https://wiki.centos.org/HowTos/SELinux

The build script is now more robust and provides better error messages to help you diagnose and fix issues. If you follow the setup steps and check the troubleshooting guide, you should be able to resolve most common build issues.