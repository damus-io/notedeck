fn main() {
    // On macOS, wasmer's compact unwind manager references
    // __unw_add_find_dynamic_unwind_sections, a private Apple libunwind
    // symbol that isn't always available to the linker. Provide a weak
    // no-op stub so the link succeeds; wasmer already handles the symbol
    // being absent at runtime.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let out = std::env::var("OUT_DIR").unwrap();
        let stub_path = format!("{out}/unwind_stub.c");
        std::fs::write(
            &stub_path,
            b"__attribute__((weak)) void __unw_add_find_dynamic_unwind_sections(void) {}\n",
        )
        .unwrap();
        cc::Build::new().file(&stub_path).compile("unwind_stub");
    }
}
