# Diskoria — release process

Two repositories are involved and they hold different things:

| Repo | Holds |
|------|-------|
| `steeb-k/diskoria` (this one, `origin`) | **source**. Every release gets a git **tag** and a GitHub Release here. |
| `steeb-k/diskoria-binaries` | **artifacts**. `diskoria.exe` + `diskoria-<ver>-setup.exe` attached to a release. The in-app updater reads this repo. |

`releases/` is **gitignored** — built artifacts are never committed to the source
repo. The only durable copy of a shipped binary is the one attached to the
diskoria-binaries release, so treat that as the archive.

## Why the tag matters

1.6.1 was built, published to diskoria-binaries, and **never tagged here**. The
source repo therefore had no marker that 1.6.1 existed, `releases/1.6.1/` looked
like a stale local build, and it was rebuilt from newer code — producing a second
different binary claiming to be 1.6.1. Tag at release time and that cannot
happen: `git tag` is the record of what shipped.

## Checklist

1. **Land the work.** Everything merged to `main`, `cargo clippy --all-targets`
   and `DISKORIA_SKIP_RESOURCE=1 cargo test` clean, boot smoke passing.
2. **Bump the version** in `diskoria/Cargo.toml` (`scripts/set-version.ps1`, or
   edit `[package] version` — that is all the script does). Nothing else carries
   the version; `build-release.ps1` reads it from `cargo metadata` and passes it
   to Inno Setup.

   `diskoria/Cargo.lock` is **tracked** and carries the version too, but only
   rewrites itself the next time cargo runs — so committing straight after the
   edit leaves it behind, and the release build then dirties the tree *after*
   the tag. Run `cargo metadata --no-deps` (or any cargo command) in `diskoria\`
   and commit both files together.
3. **Build unsigned and hand off for manual testing**:
   ```powershell
   .\scripts\build-release.ps1 -NoSign
   ```
   Never sign or publish before a human has tested the build. Test the
   **installed** build, not just the portable exe — the two differ in
   `close_to_tray`, autostart and update support. Close any running portable copy
   first, tray icon included, or the single-instance guard will hand your
   installed launch to it (KI-28); the startup banner logs which build you are
   actually looking at.
4. **Refresh the wiki captures** if the UI moved. Dark theme only; use demo mode
   so no real hardware is shown:
   ```powershell
   cargo run -- --page settings --demo-theme dark
   ```
5. **Sign** once the build is approved:
   ```powershell
   .\scripts\build-release.ps1 -Sign
   ```
   Requires `scripts/artifact-signing-metadata.json` (Azure Artifact Signing).
   `-Sign` fails the build rather than silently shipping unsigned.
6. **Tag the source and cut the GitHub Release here.** Bare version, no `v`
   prefix, matching the existing `1.6.0` tag:
   ```powershell
   git tag 1.6.2
   git push origin main --tags
   gh release create 1.6.2 --title "1.6.2" --notes-file <notes>
   ```
7. **Publish the artifacts on diskoria-binaries** — same bare tag — attaching
   both `diskoria.exe` and `diskoria-<ver>-setup.exe` from `releases\<ver>\`.
   The updater strips an optional leading `v`, but keep tags bare on both repos.
8. **Verify the update path** from the previous release, and remember
   **KI-22**: the self-update bug shipped in 1.6.0, so upgrading *from* 1.6.0
   must be done by running the installer manually. Update checks are
   installed-only by design (KI-23).

## Release notes

Draw them from `docs/known-issues.md` — each fix moved to **Resolved** in that
release is a line. Keep KI numbers in the notes; they are stable identifiers and
make it possible to trace a user report back to the entry.

No emoji. Plain, professional English: no decorative symbols, no marketing voice,
no call-to-action headings. State what went wrong, then what it does now.

```powershell
gh release view <tag> --repo <repo> --json body --jq '.body'
gh release edit <tag> --repo <repo> --notes-file <file>   # to correct it
```

## Linux release assets

`./scripts/build-portable.sh` builds and emits, under `releases/<ver>/`:

- `diskoria-<ver>-linux-<arch>` — the bare binary. **This is the asset the
  Linux self-updater downloads** (`update::pick_linux_url` matches on
  `linux` + arch and skips archives/checksums), so upload it un-archived.
- `diskoria-<ver>-portable-linux-<arch>.tar.gz` — binary + polkit policy +
  `.desktop` + `diskoria-monitor.service` + `install-service.sh` + README,
  for humans downloading manually.

Upload both to the same diskoria-binaries release as the Windows assets.
No signing story exists for Linux yet; consider a `.sha256` sidecar (the
updater ignores it).

`scripts/install-linux.sh` — the `curl | bash` one-liner in the README — reads
the **latest** release and matches on these exact names, so a release that
publishes Windows assets alone leaves the one-liner failing with "no Linux
`<arch>` asset" until the Linux ones are attached. Publish all of them
together, or the README's install path is broken for as long as the gap lasts.

### Build once per architecture

Linux, unlike Windows, has **no x86-64-on-ARM emulation**, so an x86-64 build
simply will not execute on an ARM64 machine — there is no "universal" Linux
asset. Build both from this machine:

```sh
./scripts/build-portable.sh                                     # x86_64
./scripts/build-portable.sh --target aarch64-unknown-linux-gnu  # ARM64
```

The cross build needs, once:

```sh
rustup target add aarch64-unknown-linux-gnu
sudo pacman -S --needed aarch64-linux-gnu-gcc aarch64-linux-gnu-glibc   # Arch

# Debian/Ubuntu (verified on the WSL Ubuntu 24.04 used for the 1.7.0 build).
# All three are needed and the failures are staggered: without the C++ compiler
# the build dies partway with `failed to find tool "aarch64-linux-gnu-g++"`,
# and without the target libc it dies earlier still, in ring, with
# `bits/libc-header-start.h: No such file or directory`.
sudo apt-get install -y gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross
```

A **native** Linux build additionally needs `libfontconfig1-dev`
(plus `libxkbcommon-dev libwayland-dev pkg-config`) — see CLAUDE.md and KI-53.
The cross build does not, because `RUST_FONTCONFIG_DLOPEN=1` below removes the
compile-time dependency.

A C toolchain is required because `rusqlite` bundles SQLite. The linker is
pinned in `diskoria/.cargo/config.toml`; note cargo resolves that file relative
to the **current directory**, not `--manifest-path`, which is why the script
builds from inside the crate.

`RUST_FONTCONFIG_DLOPEN=1` is set for cross builds only: `font-kit` (via
`plotters`) locates fontconfig through pkg-config, which cannot cross-compile
without a target sysroot. Its build script's escape hatch dlopens fontconfig at
runtime instead — which also leaves the ARM binary needing nothing but
`libc`/`libm`/`libgcc_s` at load time, with X11, Wayland and fontconfig all
opened on demand.

Upload both sets:

```
diskoria-<ver>-linux-x86_64            diskoria-<ver>-linux-aarch64
diskoria-<ver>-portable-linux-x86_64.tar.gz   …-portable-linux-aarch64.tar.gz
```

The updater picks the asset matching the running machine and **refuses** one
built for a different architecture — a release that omits an architecture reads
as "no update available" to those machines rather than bricking them (KI-46).
Nothing in the crate is architecture-specific (no intrinsics; `crt-static` is
Windows-MSVC only), so an ARM build needs no source changes.

Shipping only x86-64 is a valid choice; ARM users simply never see an update.
