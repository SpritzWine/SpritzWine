# Quick Start: Using Rust in Wine Programs

## For Program Developers

To use Rust functions in your Wine program, just add ONE line to your Makefile.in:

```makefile
EXTRADLLFLAGS = -mconsole -mwinerust
```

That's it! Now you can:

1. Include the header:
   ```c
   #include "wine_lib.h"
   ```

2. Call Rust functions:
   ```c
   hello_from_rs();
   ```

## Complete Example

**programs/myapp/Makefile.in:**
```makefile
MODULE    = myapp.exe
IMPORTS   = user32

EXTRADLLFLAGS = -mconsole -mwinerust

SOURCES = \
    main.c
```

**programs/myapp/main.c:**
```c
#include <windows.h>
#include "wine_lib.h"

int main(void)
{
    MessageBoxA(NULL, "Calling Rust...", "Info", MB_OK);
    hello_from_rs();
    return 0;
}
```

That's all you need! The `-mwinerust` flag handles:
- ✅ Library paths
- ✅ Linking
- ✅ Runtime paths  
- ✅ Architecture detection

For more details, see [README_INTEGRATION.md](README_INTEGRATION.md)
