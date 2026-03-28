# Agent guidance — JF Storage Tester

This file helps AI coding agents (and humans) work effectively on this repository. It summarizes architecture, constraints, and conventions discovered in the codebase.

## Product

**JF Storage Tester** is a Windows desktop utility for **storage surface scanning** (read entire physical disk) and **speed testing** (read/write benchmarks on a selected partition). It targets IT-style workflows: physical disk list, Disk Management / DiskPart shortcuts, BitLocker awareness, and update checks against GitHub releases.

## Tech stack

| Area | Choice |
|------|--------|
| UI | **WPF** (`UseWPF`), XAML |
| Runtime | **.NET 8**, `net8.0-windows`, **Windows-only** |
| Drive enumeration | **WMI** via `System.Management`, `Microsoft.Management.Infrastructure` |
| Low-level I/O | **P/Invoke** to `kernel32` (`CreateFile`, `ReadFile`, unbuffered I/O, etc.) |
| Patterns | **MVVM**, singleton **services**, `INotifyPropertyChanged` |

There is **no** ASP.NET, no cross-platform UI layer — assume **Win32 + WPF** unless the project explicitly migrates (e.g. to Rust with a different UI stack).

## Repository layout

The **legacy WPF application** from upstream lives under **`_original/`** (same tree as [jjfbno/JF-Storage-Tester](https://github.com/jjfbno/JF-Storage-Tester)). New work (e.g. replacement UI or a Rust crate) can sit at the repo root alongside it.

```
_original/
  App.xaml / App.xaml.cs        Application resources, startup, theme + single-instance mutex
  MainWindow.xaml(.cs)          Shell window; DataContext = MainViewModel; title bar theme + icon
  Views/                        UserControls and dialogs (e.g. SurfaceTestView, SpeedTestView, SettingsWindow)
  ViewModels/                   BaseViewModel, RelayCommand, tab/feature VMs
  Models/                       PhysicalDiskModel, PartitionInfoModel, enums (storage type, BitLocker, …)
  Services/                     DriveDetectionService, SurfaceTestService, SpeedTestService, ThemeService, UpdateService
  Themes/                       DarkTheme.xaml, LightTheme.xaml, SharedStyles.xaml
  Converters/                   Value converters registered in App.xaml
  installer/                    Inno Setup script (`.iss`); publish output paths are relative to this tree
```

## Architecture conventions

- **ViewModels** inherit `BaseViewModel` (`SetProperty`, `OnPropertyChanged`). Commands use `RelayCommand` with `CommandManager` for `CanExecute` invalidation.
- **MainViewModel** owns **`SurfaceTestViewModel`** and **`SpeedTestViewModel`**; it wires `SelectedDrive` / `SelectedPartition`, propagates `IsTestRunning`, and subscribes to `DriveDetectionService.DrivesChanged` on the **UI dispatcher** (`Application.Current.Dispatcher.Invoke` / `BeginInvoke`).
- **Services** are typically **singletons** (`Instance` + lock): e.g. `DriveDetectionService`, `ThemeService`. Construct VMs with these in mind (do not assume DI containers unless one is added).
- **Theme**: `ThemeService` swaps the merged `*Theme.xaml` dictionary at runtime and raises `ThemeChanged`. `MainWindow` applies **DWM immersive dark mode** and swaps the window **icon** (black vs white `.ico`). New brushes/styles should live in **Themes/** and use **`DynamicResource`** where the rest of the app does.
- **Updates**: `UpdateService` uses the GitHub API for **`jjfbno/JF-Storage-Tester`** releases. If the fork or repo ownership changes, update owner/repo constants and any release pipeline expectations.

## Operational constraints (do not ignore)

1. **Administrator elevation** — `_original/app.manifest` requests `requireAdministrator`. Features that touch raw disks or perform privileged operations assume this.
2. **Single instance** — `App` holds a named mutex (`JFStorageTester_SingleInstance`) for **Inno Setup** (`AppMutex` in `.iss`) and clean upgrades. `App.ReleaseAppMutex()` exists for the update/install path. Avoid removing or renaming without updating the installer script under `_original/installer/`.
3. **Thread affinity** — Background work must **marshal to the WPF dispatcher** before touching dependency objects or collections bound to the UI. Follow existing `Dispatcher.Invoke` / `BeginInvoke` patterns in `DriveDetectionService` and view models.
4. **Disk safety** — Surface test reads **physical drives**; speed test writes/reads **files on partitions**. Preserve user-visible warnings and BitLocker-locked behavior when changing flows.

## Build & run

From the repo root, build the legacy app:

```powershell
cd _original
dotnet build
dotnet run
```

The `_original/JFStorageTester.csproj` sets **single-file**, **self-contained**, **`win-x64`** publish defaults. For a release-like binary, run `dotnet publish` from `_original/`.

**Installer**: `_original/installer/JFStorageTester.iss` references `..\publish\JFStorageTester.exe` (paths relative to that folder) — align publish output and version defines (`MyAppVersion`) when bumping versions.

## UI / AI collaboration tips

- Prefer **adding or adjusting resources** in `_original/Themes/*.xaml` and **styles** in `_original/Themes/SharedStyles.xaml` over duplicating inline styles in every view.
- **MainWindow** hosts tab content via **`ContentControl`** and `SelectedTabIndex`; new tabs need VM properties, commands, and triggers consistent with existing tabs.
- Child views bind to **child view models** exposed on `MainViewModel` (e.g. surface/speed VMs); keep that hierarchy when splitting or merging UI.
- Icons are **pack URIs** to `JF - Black.ico` / `JF - White.ico` — keep theme pairing coherent when replacing assets.

## Git remotes (common fork setup)

Contributors often use:

- **`origin`** — personal fork (push feature branches here).
- **`upstream`** — `jjfbno/JF-Storage-Tester` (fetch merges / PR base).

Adjust if your clone uses different remote names.

## Out of scope for agents unless requested

- Rewriting the whole app in another language/framework in one pass.
- Removing admin requirement or raw disk access without an explicit product decision.
- Changing the GitHub release owner/repo without confirming release workflow.

When in doubt, **match existing patterns** in the nearest ViewModel/Service, **keep UI updates on the dispatcher**, and **preserve installer and single-instance behavior** for shipping builds.
