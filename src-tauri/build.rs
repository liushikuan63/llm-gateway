fn main() {
    tauri_build::build();
    // MockRuntime command tests link dialog code, whose TaskDialogIndirect
    // import needs Common Controls v6 before main. Tauri's application manifest
    // is not automatically attached to Cargo's test executables. Keep this
    // loader requirement test-only; ordinary product targets remain unchanged.
    if std::env::var_os("CARGO_FEATURE_IPC_TESTS").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
    {
        // Cargo's -tests variant does not reach the lib unit-test executable.
        // The existing ipc-tests feature isolates these flags from product builds.
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
    }
}
