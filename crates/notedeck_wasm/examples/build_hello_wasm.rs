/// Compiles the hello WAT module to a .wasm file and places it in
/// the notedeck wasm_apps directory for testing.
///
/// Usage: cargo run -p notedeck_wasm --example build_hello_wasm
fn main() {
    let wat = r#"(module
        (import "env" "nd_heading" (func $nd_heading (param i32 i32)))
        (import "env" "nd_label"   (func $nd_label   (param i32 i32)))
        (import "env" "nd_button"  (func $nd_button   (param i32 i32) (result i32)))
        (import "env" "nd_add_space" (func $nd_add_space (param f32)))

        (memory (export "memory") 1)

        ;; Static strings
        (data (i32.const 0)  "Hello from WASM!")   ;; 16 bytes
        (data (i32.const 16) "Click me")           ;; 8 bytes
        (data (i32.const 24) "Clicks: ")           ;; 8 bytes
        (data (i32.const 32) "0123456789")         ;; digit table

        ;; App metadata
        (data (i32.const 500) "Hello WASM")
        (global (export "nd_app_name_ptr") i32 (i32.const 500))
        (global (export "nd_app_name_len") i32 (i32.const 10))

        ;; Global click counter
        (global $count (mut i32) (i32.const 0))

        ;; Simple itoa: writes decimal digits at ptr, returns length
        (func $itoa (param $n i32) (param $ptr i32) (result i32)
            (local $len i32)
            (local $tmp i32)
            (local $start i32)
            (local $end i32)
            (local $swap i32)

            ;; Handle zero
            (if (i32.eqz (local.get $n))
                (then
                    (i32.store8 (local.get $ptr) (i32.const 48)) ;; '0'
                    (return (i32.const 1))
                )
            )

            ;; Write digits in reverse
            (local.set $tmp (local.get $n))
            (local.set $len (i32.const 0))
            (block $done
                (loop $digits
                    (br_if $done (i32.eqz (local.get $tmp)))
                    (i32.store8
                        (i32.add (local.get $ptr) (local.get $len))
                        (i32.add (i32.const 48)
                            (i32.rem_u (local.get $tmp) (i32.const 10))))
                    (local.set $tmp (i32.div_u (local.get $tmp) (i32.const 10)))
                    (local.set $len (i32.add (local.get $len) (i32.const 1)))
                    (br $digits)
                )
            )

            ;; Reverse the digits in place
            (local.set $start (i32.const 0))
            (local.set $end (i32.sub (local.get $len) (i32.const 1)))
            (block $rev_done
                (loop $rev
                    (br_if $rev_done (i32.ge_u (local.get $start) (local.get $end)))
                    ;; swap
                    (local.set $swap
                        (i32.load8_u (i32.add (local.get $ptr) (local.get $start))))
                    (i32.store8
                        (i32.add (local.get $ptr) (local.get $start))
                        (i32.load8_u (i32.add (local.get $ptr) (local.get $end))))
                    (i32.store8
                        (i32.add (local.get $ptr) (local.get $end))
                        (local.get $swap))
                    (local.set $start (i32.add (local.get $start) (i32.const 1)))
                    (local.set $end (i32.sub (local.get $end) (i32.const 1)))
                    (br $rev)
                )
            )

            (local.get $len)
        )

        (func (export "nd_update")
            (local $num_len i32)

            ;; Heading
            (call $nd_heading (i32.const 0) (i32.const 16))
            (call $nd_add_space (f32.const 8.0))

            ;; Button
            (if (call $nd_button (i32.const 16) (i32.const 8))
                (then
                    (global.set $count
                        (i32.add (global.get $count) (i32.const 1)))
                )
            )
            (call $nd_add_space (f32.const 4.0))

            ;; "Clicks: " prefix is at offset 24 (8 bytes)
            ;; Write the number starting at offset 100 (scratch space)
            (local.set $num_len
                (call $itoa (global.get $count) (i32.const 100)))

            ;; Copy "Clicks: " to offset 200, then append the number
            ;; offset 200: "Clicks: "
            (memory.copy (i32.const 200) (i32.const 24) (i32.const 8))
            ;; offset 208: number digits
            (memory.copy
                (i32.const 208)
                (i32.const 100)
                (local.get $num_len))

            ;; Label with total length = 8 + num_len
            (call $nd_label
                (i32.const 200)
                (i32.add (i32.const 8) (local.get $num_len)))
        )
    )"#;

    let wasm = wat::parse_str(wat).expect("failed to parse WAT");

    // Write to the notedeck wasm_apps directory
    let dir = dirs::data_dir()
        .expect("data dir")
        .join("notedeck")
        .join("cache")
        .join("wasm_apps");
    std::fs::create_dir_all(&dir).expect("create wasm_apps dir");

    let path = dir.join("hello.wasm");
    std::fs::write(&path, &wasm).expect("write hello.wasm");
    println!("Wrote {} bytes to {}", wasm.len(), path.display());
}
