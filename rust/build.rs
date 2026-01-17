// Unified build script for Wine Rust integration
// Handles both library and binary build requirements

use std::env;

fn main() {
    println!("cargo:warning==== Wine Rust Unified Build Script ===");
    println!("cargo:warning=Handling both library and binary requirements");

    // Get the workspace root - we need to go up to find the Wine source
    let workspace = env!("CARGO_MANIFEST_DIR");
    let root = if workspace.ends_with("/rust") {
        format!("{workspace}/../wine/")
    } else {
        format!("{workspace}/../../wine/")
    };

    println!("cargo:warning=Workspace root: {}", root);

    // Check if we're building the library (src_lib) or main binary
    let is_library = env::var("CARGO_PKG_NAME").map_or(false, |name| name == "wine_api");

    if is_library {
        println!("cargo:warning=📦 Building Wine API library");
        build_library(&root);
    } else {
        println!("cargo:warning=🚀 Building Wine Rust binary");
        build_binary(&root);
    }

    println!("cargo:warning=✅ Wine Rust build configuration complete");
}

/// Build configuration for the Wine API library
fn build_library(root: &str) {
    // Set up library search paths
    let wine_libs = [
        ("ntdll", "ntdll.so"), // Use ntdll.so as it contains the debug functions
        ("kernel32", "kernel32.dll.so"),
        ("user32", "user32.dll.so"),
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
}

/// Build configuration for the main binary
fn build_binary(root: &str) {
    // For the binary, we only need ntdll.so (contains debug functions)
    let lib_name = "ntdll";
    let lib_file = "ntdll.so";
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
