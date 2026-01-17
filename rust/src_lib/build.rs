// Build script for the Wine API library
// This handles library-specific build requirements including bindgen

use cbindgen;
use std::{env, path::PathBuf};

struct Library {
    headers: Box<Vec<&'static str>>,
    blocklist_types: Box<Vec<&'static str>>,
}

impl Library {
    fn new(headers: Vec<&'static str>) -> Self {
        Library {
            headers: Box::new(headers),
            blocklist_types: Box::new(vec![]),
        }
    }

    fn block_type(mut self, type_name: &'static str) -> Self {
        self.blocklist_types.push(type_name);
        self
    }
}

impl Default for Library {
    fn default() -> Self {
        Library {
            headers: Box::new(vec![]),
            blocklist_types: Box::new(vec![]),
        }
    }
}

fn main() {
    println!("cargo:warning==== Wine Rust Library Build Script ===");
    println!("cargo:warning=Handling library-specific requirements");

    // Get the workspace root - we need to go up to find the Wine source
    let workspace = env!("CARGO_MANIFEST_DIR");
    let root = if workspace.ends_with("/src_lib") {
        format!("{workspace}/../../wine/")
    } else {
        format!("{workspace}/../wine/")
    };

    println!("cargo:warning=Workspace root: {}", root);

    // Set up library search paths for Wine libraries
    let wine_libs = [
        ("ntdll", "ntdll.so"), // Use ntdll.so as it contains the debug functions
        ("kernel32", "kernel32.dll.so"),
        ("user32", "user32.dll.so"),
        ("httpapi", "httpapi.dll.so"), // Add HTTP Server API
    ];

    for (lib_name, lib_file) in &wine_libs {
        let lib_path = format!("{root}dlls/{}/{lib_file}", lib_name);
        if std::path::Path::new(&lib_path).exists() {
            println!(
                "cargo:warning=📁 Found Wine library: {} at {}",
                lib_name, lib_path
            );

            // Add the directory containing the .so file
            let dll_dir = format!("{root}dlls/{}", lib_name);
            println!("cargo:rustc-link-search={}", dll_dir);

            // Add rpath for runtime linking
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dll_dir);

            // Link against the specific .so file
            println!("cargo:rustc-link-arg=-l:{}", lib_file);
        } else {
            println!(
                "cargo:warning=⚠️  Wine library {} not found at {}",
                lib_name, lib_path
            );
        }
    }

    // Also add the root dlls directory for completeness
    println!("cargo:rustc-link-search={root}dlls");

    // Generate Rust bindings using bindgen
    generate_bindings(&root);

    generate_cbindings();

    println!("cargo:warning=✅ Wine Rust library build configuration complete");
}

/// Generate Rust bindings for Wine C APIs using bindgen
fn generate_bindings(root: &str) {
    println!("cargo:warning=🔧 Generating Rust bindings with bindgen...");

    // Check if LIBCLANG_PATH is set, if not try to auto-detect
    let libclang_found = if env::var("LIBCLANG_PATH").is_err() {
        let libclang_paths = [
            "/usr/lib64/llvm20/lib64",
            "/usr/lib64/rocm/llvm/lib",
            "/usr/lib64/llvm21/lib64",
            "/usr/lib64",
            "/usr/lib/llvm-20/lib",
            "/usr/lib/llvm-21/lib",
            "/usr/lib/llvm-19/lib",
            "/usr/lib/llvm-18/lib",
            "/usr/lib/llvm-17/lib",
            "/usr/lib/llvm-16/lib",
            "/usr/lib/llvm-15/lib",
            "/usr/lib/llvm-14/lib",
            "/usr/lib/llvm/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/usr/local/opt/llvm/lib",
            "/opt/homebrew/opt/llvm/lib",
        ];

        let mut found = false;
        for path in libclang_paths {
            if std::path::Path::new(path).exists() {
                println!("cargo:warning=🔍 Found libclang at: {}", path);
                unsafe {
                    env::set_var("LIBCLANG_PATH", path);
                }
                found = true;
                break;
            }
        }
        found
    } else {
        println!("cargo:warning=🔧 Using LIBCLANG_PATH from environment");
        true
    };

    if !libclang_found {
        println!("cargo:warning=❌ libclang not found! Bindings generation may fail.");
        println!("cargo:warning=💡 Try: sudo dnf install llvm libclang-devel");
    }

    let debug: Library = Library::new(vec![
        "include/wine/debug",
        "include/winbase",
        "include/winnt",
        "include/winuser",
    ]);
    let http = Library::new(vec![
        "include/http",
        "include/wine/http",
        "../rust/src_lib/includes",
        "include/winhttp",
        "include/windows",
    ])
    .block_type("WINHTTP_CONNECTION_INFO");

    let libraries: Vec<Library> = vec![debug, http];

    let builder = libraries
        .iter()
        .fold(bindgen::Builder::default(), |mut b, library| {
            b = library
                .headers
                .iter()
                .map(|h| format!("{root}{h}.h"))
                .fold(b, |bu, h| bu.header(h));

            b = library
                .blocklist_types
                .iter()
                .fold(b, |bu, h| bu.blocklist_type(h));

            b
        });

    // Generate bindings for core Wine headers
    let bindings = match builder
        // Core Wine headers
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .clang_arg(format!("-I{root}include"))
        .clang_arg(format!("-I{root}include/msvcrt"))
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: false,
        })
        .use_core()
        .prepend_enum_name(false)
        .size_t_is_usize(false)
        .layout_tests(false)
        .clang_arg("-D__stdcall=__attribute__((stdcall))")
        .clang_arg("-D__cdecl=__attribute__((cdecl))")
        .clang_arg("-D__fastcall=__attribute__((fastcall))")
        .clang_arg("-D_WIN32")
        .clang_arg("-D__WINE__")
        .generate()
    {
        Ok(bindings) => bindings,
        Err(e) => {
            println!("cargo:warning=❌ Failed to generate bindings: {}", e);
            panic!("Bindings generation failed");
        }
    };

    // Write the bindings to the generated directory
    let out_path = PathBuf::from(format!("{}/generated/", env!("CARGO_MANIFEST_DIR")));
    std::fs::create_dir_all(&out_path).unwrap();
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    println!("cargo:warning=✅ Successfully generated FFI bindings");
}

fn generate_cbindings() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_language(cbindgen::Language::C)
        .with_documentation(true)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file("generated/exports.h");
}
