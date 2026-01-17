# Wine Rust API Integration Guide

This document explains how to integrate Rust code into Wine programs using the Wine Rust API.

## Overview

The Wine Rust API (`wine_api`) provides a way to call Rust functions from Wine programs. The integration is automatic and requires minimal changes to your program's Makefile.

## Quick Start

### 1. Add `-mwinerust` to Your Program's Makefile

In your program's `Makefile.in`, add the `-mwinerust` flag to `EXTRADLLFLAGS`:

```makefile
MODULE    = myprogram.exe
IMPORTS   = user32 kernel32

EXTRADLLFLAGS = -mconsole -mwinerust

SOURCES = \
    main.c
```

### 2. Include the Wine Rust API Header

In your C source files, include `wine_lib.h`:

```c
#include "wine_lib.h"
```

### 3. Call Rust Functions

You can now call any exported Rust function:

```c
#include <stdio.h>
#include "wine_lib.h"

int main(void)
{
    char result = hello_from_rs();
    printf("Rust returned: %c\n", result);
    return 0;
}
```

## How It Works

### The `-mwinerust` Flag

The `-mwinerust` flag is a custom Wine compiler flag that:
- Automatically adds the correct library search paths for the Rust library
- Links your program against `libwine_api.so`
- Sets up the runtime library path (rpath) so the library can be found at runtime
- Detects the correct architecture (i686, x86_64, aarch64, arm) automatically

### Architecture Support

The Wine Rust API automatically builds and links the correct architecture:
- **32-bit x86**: `i686-unknown-linux-gnu`
- **64-bit x86**: `x86_64-unknown-linux-gnu`
- **ARM**: `armv7-unknown-linux-gnueabihf`
- **ARM64**: `aarch64-unknown-linux-gnu`

The correct target is determined during Wine's configure process.

## Examples

### Example: ping Program

The `programs/ping` directory shows a working example:

**Makefile.in:**
```makefile
MODULE    = ping.exe
IMPORTS   = ws2_32 iphlpapi

EXTRADLLFLAGS = -mconsole -mwinerust

SOURCES = \
    ping_main.c
```

**ping_main.c:**
```c
#include "wine/debug.h"
#include "../../rust/wine_lib.h"

int main(int argc, char** argv)
{
    hello_from_rs();  // Call Rust function
    
    // Rest of ping implementation...
}
```

## Adding New Rust Functions

### 1. Edit the Rust Source

Add your function to `rust/src_lib/src/rswine.rs`:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn my_new_function(value: i32) -> i32 {
    println!("Rust function called with: {}", value);
    value * 2
}
```

### 2. Rebuild the Rust Library

```bash
cd rust/src_lib
cargo build --release --target i686-unknown-linux-gnu
```

### 3. The Function is Automatically Exported

The function will be automatically added to `rust/src_lib/generated/exports.h` by cbindgen during the build process.

### 4. Use It in Your C Code

```c
#include "wine_lib.h"

int result = my_new_function(42);
```

## Build Requirements

- **Rust toolchain**: Required with the appropriate target installed
  ```bash
  rustup target add i686-unknown-linux-gnu  # for 32-bit Wine
  rustup target add x86_64-unknown-linux-gnu  # for 64-bit Wine
  ```

- **libclang**: Required for bindgen (FFI generation)
  ```bash
  # Fedora/RHEL
  sudo dnf install clang-libs
  
  # Ubuntu/Debian
  sudo apt install libclang-dev
  ```

## Configuration Variables

The following variables are set during Wine's configure process:

- `RUST_TARGET`: The Rust target triple (e.g., `i686-unknown-linux-gnu`)
- `WINE_API_DLLFLAGS`: Set to `-mwinerust` for easy use in Makefiles
- `WINE_API_LDFLAGS`: The raw linker flags (for advanced use)

## Troubleshooting

### Library Not Found at Runtime

If you get "cannot find libwine_api.so" errors, ensure:
1. The Rust library was built: `ls rust/target/*/release/deps/libwine_api.so`
2. The rpath is set correctly: `readelf -d program.exe.so | grep RUNPATH`

### Wrong Architecture

If the wrong architecture is being used:
1. Check: `echo $RUST_TARGET` in the build environment
2. Verify Wine's host architecture: `grep "^HOST_ARCH" Makefile`

### Missing Rust Functions

If a Rust function isn't visible:
1. Ensure it has `#[unsafe(no_mangle)]` and `pub extern "C"`
2. Check exports.h: `cat rust/src_lib/generated/exports.h`
3. Rebuild the Rust library with `cargo build --release`

## Advanced Usage

### Using WINE_API_LDFLAGS Directly

If you need more control, you can use `WINE_API_LDFLAGS` instead of `-mwinerust`:

```makefile
EXTRADLLFLAGS = -mconsole $(WINE_API_LDFLAGS)
```

This provides the same functionality but shows the actual linker flags being used.

### Custom Rust Library Path

Set the `WINE_OBJDIR` environment variable to override the Wine source directory:

```bash
export WINE_OBJDIR=/path/to/wine/source
make
```

## License

The Wine Rust API integration follows Wine's LGPL 2.1+ license.