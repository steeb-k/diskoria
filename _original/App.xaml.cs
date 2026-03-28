using System.Threading;
using System.Windows;
using JFStorageTester.Services;

namespace JFStorageTester;

public partial class App : Application
{
    private ThemeService? _themeService;
    private Mutex? _appMutex;

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        
        // Create named mutex so Inno Setup can detect the running instance
        _appMutex = new Mutex(true, "JFStorageTester_SingleInstance", out _);
        
        // Initialize theme service and apply Windows theme
        _themeService = ThemeService.Instance;
        _themeService.ApplyWindowsTheme();
        _themeService.StartMonitoringThemeChanges();
    }

    /// <summary>
    /// Release the app mutex so the installer doesn't think we're still running.
    /// Must be called before launching the update installer.
    /// </summary>
    public static void ReleaseAppMutex()
    {
        if (Current is App app && app._appMutex != null)
        {
            try { app._appMutex.ReleaseMutex(); } catch { }
            app._appMutex.Dispose();
            app._appMutex = null;
        }
    }

    protected override void OnExit(ExitEventArgs e)
    {
        _themeService?.StopMonitoringThemeChanges();
        if (_appMutex != null)
        {
            try { _appMutex.ReleaseMutex(); } catch { }
            _appMutex.Dispose();
            _appMutex = null;
        }
        base.OnExit(e);
    }
}
