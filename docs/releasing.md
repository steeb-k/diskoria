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
