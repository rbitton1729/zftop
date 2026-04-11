# zftop

A terminal-based dashboard for the Zettabyte File System, in the spirit of `htop`.

## Status

**v0.2.** zftop now has three tabs: an Overview that summarizes memory + ARC + pools on one screen, a Pools view with capacity / health / vdev trees / scrub status, and the v0.1 ARC dashboard unchanged. Datasets, snapshots, and SMART are still coming in v0.3–v0.5.

The reason zftop exists is that the existing tools each give you one slice of the picture (`zpool status`, `arc_summary`, `zfs list`, `smartctl`), and you end up running four commands and holding the whole thing in your head. zftop is the dashboard that fuses them.

## What v0.2 shows

Three tabs, each refreshing once a second:

- **Overview** — a compact alarm summary. System RAM bar, ARC gauge with hit ratios, compact pools list with health and capacity. One screen to answer "is anything wrong."
- **Pools** — full pool inventory with a moving selection. Wide layout (≥100 cols) shows name / health / capacity+minibar / fragmentation / scrub state / error count. Press `Enter` on a pool to drill into a detail view with the full `zpool status`-style vdev tree (raidz / mirror / disks / logs / cache / spares).
- **ARC** — the v0.1 screen, unchanged, now also reachable via tab `3`:
    - **System RAM**: a colored bar showing how memory is distributed across the running system. On Linux: App / ARC / Buf-Cache. On FreeBSD: Wired / ARC / Active / Inactive. Free is the empty bar tail.
    - **ARC size**: current size against `c_max`, as a gauge, so you can see at a glance whether the ARC is near its ceiling.
    - **Breakdown**: MFU data, MRU data, metadata, headers, dbuf, dnode, bonus. Each shown both in bytes and as a percentage of total ARC.
    - **Hit ratios**: overall, demand, and prefetch.
    - **ARC compression**: ratio plus the uncompressed-to-compressed sizes, so you can see if your `compression=lz4`/`zstd` is actually pulling its weight.
    - **Throughput**: hits, IO hits, and misses per second.

ARC / memory data comes from `/proc/spl/kstat/zfs/arcstats` + `/proc/meminfo` on Linux, or `sysctl kstat.zfs.misc.arcstats.*` + `sysctl vm.stats.vm.*` on FreeBSD. Pool data (topology, scrub state, error counts) is read directly through `libzfs` — no shelling out to `zpool`, no parsing of human-formatted CLI output.

## How the RAM bar math works

On Linux:

- **User+Kernel** = `MemTotal − MemFree − (Buffers + Cached + SReclaimable) − ARC footprint`
- **ARC** = `arcstats.size + arcstats.overhead_size`. The second term is ABD scatter waste plus compression bookkeeping — real RAM that ZFS holds but that isn't counted in `size`.
- **Buf/Cache** = `Buffers + Cached + SReclaimable`
- Empty tail = `MemFree`. Smaller than htop's empty tail, which uses `MemAvailable`. zftop reports what's currently held, not what could be reclaimed under pressure.
- **ZFS available** (right side of the bar) = `arcstats.memory_available_bytes`, ZFS's own reclaim estimate, so you can reconcile zftop's "almost full" bar against htop's "lots of free."

FreeBSD uses `Wired` / `ARC` / `Active` / `Inactive+Laundry` instead, because its memory accounting is page-based rather than category-based.

## Install

### Quick install (Linux and FreeBSD)

```
curl -fsSL https://git.skylantix.com/rbitton/zftop/-/raw/main/install.sh | sh
```

POSIX `sh` script — no bash, no `jq`, no Python. It detects your OS and
architecture, pulls the matching binary from the latest release, verifies its
SHA-256 against the published checksum, and drops it into `/usr/local/bin`
(using `sudo` if that directory isn't writable). If you'd rather read the
script before running it:

```
curl -fsSL https://git.skylantix.com/rbitton/zftop/-/raw/main/install.sh -o install.sh
less install.sh
sh install.sh
```

Overrides:

```
sh install.sh --version 0.1.0       # pin a specific release
sh install.sh --dir ~/.local/bin    # install somewhere other than /usr/local/bin
```

Supported targets: `linux/amd64`, `linux/arm64`, `freebsd/amd64`. macOS and
FreeBSD arm64 aren't published and the script will tell you so instead of
silently installing the wrong binary.

### Arch Linux (AUR)

```
yay -S zftop
```

Or with any AUR helper. The package installs the binary as `zftop`.

### Prebuilt binary

Binaries are attached to every [release](https://git.skylantix.com/rbitton/zftop/-/releases):

- `zftop-linux-amd64`: Linux x86_64 — dynamically linked against glibc 2.31+ and libzfs
- `zftop-linux-arm64`: Linux aarch64 — dynamically linked against glibc 2.31+ and libzfs (Graviton, Ampere Altra, Pi 4/5)
- `zftop-freebsd-amd64`: FreeBSD amd64 (built on FreeBSD 15, dynamically links against system libc and base libzfs)

Download the one for your platform, then:

```
chmod +x zftop-linux-amd64
sudo mv zftop-linux-amd64 /usr/bin/zftop
```

(On FreeBSD, the conventional install path is `/usr/local/bin/zftop`.)

### Cargo / crates.io

```
cargo install zftop
```

Requires a Rust toolchain (1.85 or newer for edition 2024). Builds zftop from source on your machine and drops the binary in `~/.cargo/bin/zftop`. Works on both Linux and FreeBSD.

### From source

```
git clone https://git.skylantix.com/rbitton/zftop.git
cd zftop
cargo build --release
sudo install -Dm755 target/release/zftop /usr/bin/zftop
```

### FreeBSD

Same recipe: `pkg install rust && cargo build --release && install -m 755 target/release/zftop /usr/local/bin/zftop`. zftop reads ZFS state via `sysctl kstat.zfs.misc.arcstats.*` and memory via `sysctl vm.stats.vm.* hw.physmem hw.pagesize`, so it works out of the box on any FreeBSD with OpenZFS (vanilla FreeBSD, TrueNAS, pfSense, anything). The `--source` and `--meminfo` flags are Linux-only and ignored on FreeBSD.

## Usage

```
zftop                    # default: poll every 1s
zftop -n 500             # poll every 500ms
zftop --interval 2000    # poll every 2 seconds
zftop --help             # show all options
```

## Controls

| Key | Action |
|-----|--------|
| `q` / `Ctrl+C` | quit |
| `r` | force refresh |
| `1` / `2` / `3` | switch to Overview / Pools / ARC tab |
| `Tab` / `Shift+Tab` | cycle tabs forward / back |
| `↑` / `↓` / `j` / `k` | (Pools list) move selection |
| `Home` / `End` | (Pools list) jump to first / last |
| `Enter` | (Pools list) drill into detail view |
| `Esc` / `Backspace` | (Pools detail) back to the list |

## Requirements

- **Linux** with OpenZFS installed. The kernel module must be loaded so that `/proc/spl/kstat/zfs/arcstats` exists and `/dev/zfs` is accessible. Distro-agnostic; works on Arch, Debian, Ubuntu, NixOS, anything that ships OpenZFS.
- **or FreeBSD 14+** with OpenZFS. Works out of the box on vanilla FreeBSD, TrueNAS Core/SCALE, FreeNAS, pfSense, OPNsense, and anything else built on a recent FreeBSD base.
- **libzfs at runtime.** v0.2+ reads pool topology and vdev state directly through `libzfs`, so the shared library has to be installed and its soname has to match what the binary was linked against. On Debian/Ubuntu the package is `libzfs4linux` (pulled in by `zfsutils-linux`); on Arch it's `zfs-utils`; on Fedora it's `libzfs5` (pulled in by `zfs`). On FreeBSD 14+ libzfs is in base — nothing to install. If the prebuilt binary fails to launch with `error while loading shared libraries: libzfs.so.N`, your distro's ZFS package isn't installed or ships a different soname than our build host — fall back to `cargo install zftop` in that case.
- **Linux glibc 2.31 or newer** for the prebuilt binary. Covers Debian 11+, Ubuntu 20.04+, RHEL/Rocky/Alma 9+, recent Fedora, Arch (rolling), and recent NixOS. Older glibc systems (RHEL 8, Ubuntu 18.04) and musl-libc systems (Alpine, Void musl, Chimera, Adelie) are not supported by the prebuilt binary — use `cargo install zftop` instead, which compiles against your system's libc.
- A terminal that supports ANSI colors and box-drawing characters, i.e. any terminal made in the last 30 years.

The Linux binaries dynamically link against glibc (2.31 minimum) and libzfs; the FreeBSD binary dynamically links against the FreeBSD base libc and base libzfs. Both are drop-in installs beyond your distribution's ZFS package being installed.

## Roadmap

zftop is a *finishable* project. ZFS is stable, the surface area we care about isn't growing, and once the dashboard shows everything worth seeing there's no v3.0 plugin system to chase. The plan is to ship a few focused versions and then stop. The per-version targets below are intentions, not commitments; what actually lands where may shift as I work through them.

- **v0.1** ARC memory visualization ✓
- **v0.2** pools view: capacity, fragmentation, health, vdev tree, scrub status ✓ (this release)
- **v0.3** datasets view: usage, compression ratios, sortable and filterable
- **v0.4** snapshots view, with awareness of Sanoid retention classes
- **v0.5** SMART health joined to vdev members on the pools view
- **v1.0** Remote/Fleet mode (use ssh either independently or with Ansible to monitor many machines at once)

## License

GPL v3 or later. See `LICENSE`.
