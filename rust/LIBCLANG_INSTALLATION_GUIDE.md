# libclang Installation Guide

This guide helps you install libclang, which is required for generating FFI bindings with bindgen.

## The Error You're Seeing

```
thread 'main' panicked at 'Unable to find libclang: "couldn't find any valid shared libraries matching: ['libclang.so', 'libclang-*.so', 'libclang.so.*', 'libclang-*.so.*']"'
```

This means the build script cannot find libclang on your system.

## Solutions

### 1. Install LLVM (Recommended)

#### On Ubuntu/Debian:
```bash
sudo apt-get update
sudo apt-get install -y llvm libclang-dev clang
```

#### On Fedora/RHEL/CentOS:
```bash
sudo dnf install -y llvm clang-devel clang
```

#### On Arch Linux:
```bash
sudo pacman -S llvm clang
```

#### On macOS (Homebrew):
```bash
brew install llvm
```

### 2. Set LIBCLANG_PATH Manually

If you already have libclang installed but in a non-standard location:

```bash
# Find where libclang is installed
find /usr -name "libclang.so*" 2>/dev/null

# Set the path (example)
export LIBCLANG_PATH=/usr/lib/llvm-14/lib

# Then build
cd rust
cargo build
```

### 3. Use a Specific LLVM Version

If you need a specific version:

```bash
# Install specific version
sudo apt-get install -y llvm-14 libclang-14-dev

# Set the path
export LIBCLANG_PATH=/usr/lib/llvm-14/lib
```

### 4. Docker Solution

If you're using Docker, you can install the dependencies in your container:

```dockerfile
FROM rust:latest

# Install LLVM and clang
RUN apt-get update && apt-get install -y \
    llvm \
    libclang-dev \
    clang \
    && rm -rf /var/lib/apt/lists/*

# Set environment variable
ENV LIBCLANG_PATH=/usr/lib/llvm-14/lib
```

## Verifying Installation

Check if libclang is available:

```bash
# Check for libclang
ls /usr/lib/llvm*/lib/libclang.so*

# Check if bindgen can find it
cargo build -v 2>&1 | grep libclang
```

## Common libclang Locations

The build script automatically searches these locations:
- `/usr/lib64/llvm20/lib64`
- `/usr/lib/llvm-20/lib`
- `/usr/lib/llvm-19/lib`
- `/usr/lib/llvm-18/lib`
- `/usr/lib/llvm-17/lib`
- `/usr/lib/llvm-16/lib`
- `/usr/lib/llvm-15/lib`
- `/usr/lib/llvm-14/lib`
- `/usr/lib/llvm/lib`
- `/usr/lib/x86_64-linux-gnu`
- `/usr/local/opt/llvm/lib`
- `/opt/homebrew/opt/llvm/lib`

## Troubleshooting

### "Command not found" errors
If you get `apt-get: command not found` or similar:
- **Fedora/RHEL**: Use `dnf` instead of `apt-get`
- **Arch**: Use `pacman` instead of `apt-get`
- **macOS**: Use `brew` instead of `apt-get`

### Permission denied
Use `sudo` before installation commands if you get permission errors.

### Still not working?
1. Check if libclang is actually installed:
   ```bash
   find /usr -name "libclang.so*" 2>/dev/null
   ```

2. Try setting LIBCLANG_PATH explicitly:
   ```bash
   export LIBCLANG_PATH=/path/to/libclang.so/directory
   ```

3. Check the build output for specific error messages.

## Alternative: Skip FFI (Not Recommended)

If you absolutely cannot install libclang, you would need to modify the build script to skip FFI generation entirely. However, this would defeat the purpose of the Wine Rust integration, which is specifically designed to provide FFI bindings.

## Getting Help

If you're still having issues:
1. Check the exact error message in the build output
2. Verify your system has the required dependencies
3. Try setting LIBCLANG_PATH manually
4. Consult your distribution's documentation for LLVM installation

The build script now provides detailed error messages to help you diagnose and fix libclang issues.
