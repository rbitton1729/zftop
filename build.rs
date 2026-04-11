// Emit the libzfs link flag on every target that has libzfs.
//
// zftop v0.2c onward requires libzfs at link time and runtime. On Linux the
// distro package providing libzfs.so is typically `libzfs2linux` or
// `zfsutils-linux` (Debian/Ubuntu) or `zfs-utils` (Arch); the dev package
// supplying the headers used at build time is `libzfs-dev` (Debian/Ubuntu).
// On FreeBSD 14+, libzfs is in base at `/lib/libzfs.so.4` and no install
// is required.
//
// We unconditionally emit the link flag for Linux and FreeBSD targets. Any
// other target (macOS, Windows, etc.) is unsupported — main.rs already
// errors out of `build_sources` on unknown OSes, and the link flag omission
// here keeps the build graph honest about that.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "linux" | "freebsd" => {
            // libzfs for zpool_* symbols, libnvpair for nvlist_lookup_*.
            // On Linux these are separate .so files (libzfs2linux +
            // libnvpair3linux on Debian, zfs-utils provides both on Arch);
            // on FreeBSD base, libnvpair is inside libzfs.a but the .so
            // split mirrors Linux. Emitting both links is correct on both
            // targets — a stray -lnvpair is a no-op if already pulled in.
            println!("cargo:rustc-link-lib=zfs");
            println!("cargo:rustc-link-lib=nvpair");
        }
        _ => {
            // Non-supported target — main.rs's `build_sources` cfg-gated
            // fallback errors out at runtime. No link flag needed.
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
