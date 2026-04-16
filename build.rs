// Build-time linker configuration.
//
// zftop v0.2c+ loads libzfs and libnvpair at runtime via `dlopen` (see
// `src/pools/ffi.rs::Libzfs::load`) rather than binding them at link time.
// That means the output binary has NO libzfs / libnvpair entry in its
// `DT_NEEDED` list — the same binary works across every OpenZFS soname
// from 0.7 through 2.3+. No apt-get install libzfs-dev required in CI.
//
// The only thing build.rs has to do is ensure the dynamic loader API
// (`dlopen` / `dlsym` / `dlclose` / `dlerror`) resolves at link time:
// - glibc 2.34+ (Debian 12, Ubuntu 24.04, Arch current): dlopen lives in
//   `libc.so.6` and `-ldl` is a no-op stub. Either way works.
// - glibc 2.28–2.33 (Debian 10/11, Ubuntu 18.04/20.04, RHEL 8): dlopen
//   is in `libdl.so.2`; `-ldl` is required.
// - FreeBSD 14+: dlopen is in the base libc; `-ldl` is unnecessary but
//   a no-op link flag is harmless.
//
// Emitting `cargo:rustc-link-lib=dl` covers both cases.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "linux" => {
            // Needed for dlopen on glibc < 2.34. No-op on newer glibc.
            println!("cargo:rustc-link-lib=dl");
        }
        "freebsd" => {
            // dlopen is in base libc; no extra flag needed. Emitting a
            // cargo directive here purely for symmetry with the Linux arm.
        }
        _ => {
            // Non-supported target — main.rs's `build_sources` cfg-gated
            // fallback errors out at runtime.
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
