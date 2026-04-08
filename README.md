# zftop

A terminal-based dashboard for the Zettabyte File System, in the spirit of `htop`.

## Status

**v0.1, proof of concept.** Right now zftop does exactly one thing: it shows you, live, how much memory your ARC is using and what's inside it. That's it. No pools view, no datasets, no snapshots, no SMART. Those are coming in later versions.

The reason zftop exists is that the existing tools each give you one slice of the picture (`zpool status`, `arc_summary`, `zfs list`, `smartctl`), and you end up running four commands and holding the whole thing in your head. zftop is the dashboard that fuses them. v0.1 is the first slice.

## What v0.1 shows

A single screen that refreshes once a second:

- **System RAM**: a colored bar showing how memory is distributed across the running system. On Linux: App / ARC / Buf-Cache. On FreeBSD: Wired / ARC / Active / Inactive. Free is the empty bar tail.
- **ARC size**: current size against `c_max`, as a gauge, so you can see at a glance whether the ARC is near its ceiling.
- **Breakdown**: MFU data, MRU data, metadata, headers, dbuf, dnode, bonus. Each shown both in bytes and as a percentage of total ARC.
- **Hit ratios**: overall, demand, and prefetch.
- **ARC compression**: ratio plus the uncompressed-to-compressed sizes, so you can see if your `compression=lz4`/`zstd` is actually pulling its weight.
- **Throughput**: hits, IO hits, and misses per second.

On Linux all of this comes from `/proc/spl/kstat/zfs/arcstats` and `/proc/meminfo`. On FreeBSD it comes from `sysctl kstat.zfs.misc.arcstats.*` and `sysctl vm.stats.vm.*`. No subprocesses, no parsing of human-formatted CLI output, no surprises.

## Install

### Arch Linux (AUR)

```
yay -S zftop
```

Or with any AUR helper. The package installs the binary as `zftop`.

### Prebuilt binary

Binaries are attached to every [release](https://git.skylantix.com/rbitton/zftop/-/releases):

- `zftop-linux-amd64`: Linux x86_64 (static musl, no runtime deps)
- `zftop-linux-arm64`: Linux aarch64, static musl (Graviton, Ampere Altra, Pi 4/5)
- `zftop-freebsd-amd64`: FreeBSD amd64 (built on FreeBSD 15, dynamically links against system libc)

Download the one for your platform, then:

```
chmod +x zftop-linux-amd64
sudo mv zftop-linux-amd64 /usr/bin/zftop
```

(On FreeBSD, the conventional install path is `/usr/local/bin/zftop`.)

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

That's the whole interface in v0.1.

## Requirements

- **Linux** with OpenZFS installed. The kernel module must be loaded so that `/proc/spl/kstat/zfs/arcstats` exists. Distro-agnostic; works on Arch, Debian, Ubuntu, NixOS, anything that ships OpenZFS.
- **or FreeBSD 14+** with OpenZFS. Works out of the box on vanilla FreeBSD, TrueNAS Core/SCALE, FreeNAS, pfSense, OPNsense, and anything else built on a recent FreeBSD base. ZFS data comes from the `kstat.zfs.misc.arcstats.*` sysctls.
- A terminal that supports ANSI colors and box-drawing characters, i.e. any terminal made in the last 30 years.

No runtime dependencies beyond the kernel module being loaded. The Linux binaries are static (musl), the FreeBSD binary dynamically links only against the FreeBSD base libc. Both are drop-in installs with no package manager required.

## Roadmap

zftop is a *finishable* project. ZFS is stable, the surface area we care about isn't growing, and once the dashboard shows everything worth seeing there's no v3.0 plugin system to chase. The plan is to ship a few focused versions and then stop. The per-version targets below are intentions, not commitments; what actually lands where may shift as I work through them.

- **v0.1** ARC memory visualization (this release)
- **v0.2** pools view: capacity, fragmentation, health, vdev tree, scrub status
- **v0.3** datasets view: usage, compression ratios, sortable and filterable
- **v0.4** snapshots view, with awareness of Sanoid retention classes
- **v0.5** SMART health joined to vdev members on the pools view
- **v1.0** Remote/Fleet mode (use ssh either independently or with Ansible to monitor many machines at once)

## License

GPL v3 or later. See `LICENSE`.
