using System.Windows;
using Microsoft.Win32;

namespace JFStorageTester.Services;

public enum AppTheme
{
    Light,
    Dark
}

public class ThemeService
{
    private static ThemeService? _instance;
    private static readonly object _lock = new();
    
    public static ThemeService Instance
    {
        get
        {
            if (_instance == null)
            {
                lock (_lock)
                {
                    _instance ??= new ThemeService();
                }
            }
            return _instance;
        }
    }
    
    public event EventHandler<AppTheme>? ThemeChanged;
    
    public AppTheme CurrentTheme { get; private set; } = AppTheme.Dark;
    public bool IsFollowingSystem { get; set; } = true;
    
    private ThemeService() { }
    
    public void ApplyTheme(AppTheme theme)
    {
        CurrentTheme = theme;
        
        var themePath = theme switch
        {
            AppTheme.Light => "Themes/LightTheme.xaml",
            AppTheme.Dark => "Themes/DarkTheme.xaml",
            _ => "Themes/DarkTheme.xaml"
        };
        
        var app = Application.Current;
        if (app?.Resources.MergedDictionaries == null) return;
        
        // Find and replace the theme dictionary
        var themeDict = app.Resources.MergedDictionaries
            .FirstOrDefault(d => d.Source?.OriginalString.Contains("Theme.xaml") == true);
        
        if (themeDict != null)
        {
            var index = app.Resources.MergedDictionaries.IndexOf(themeDict);
            app.Resources.MergedDictionaries.RemoveAt(index);
            app.Resources.MergedDictionaries.Insert(index, new ResourceDictionary
            {
                Source = new Uri(themePath, UriKind.Relative)
            });
        }
        
        ThemeChanged?.Invoke(this, theme);
    }
    
    public void ApplyWindowsTheme()
    {
        var windowsTheme = GetWindowsTheme();
        ApplyTheme(windowsTheme);
    }
    
    public AppTheme GetWindowsTheme()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            
            var value = key?.GetValue("AppsUseLightTheme");
            if (value is int intValue)
            {
                return intValue == 1 ? AppTheme.Light : AppTheme.Dark;
            }
        }
        catch
        {
            // Default to dark theme if we can't read the registry
        }
        
        return AppTheme.Dark;
    }
    
    public void ToggleTheme()
    {
        IsFollowingSystem = false;
        var newTheme = CurrentTheme == AppTheme.Dark ? AppTheme.Light : AppTheme.Dark;
        ApplyTheme(newTheme);
    }
    
    public void SetTheme(AppTheme theme)
    {
        IsFollowingSystem = false;
        ApplyTheme(theme);
    }
    
    public void FollowSystemTheme()
    {
        IsFollowingSystem = true;
        ApplyWindowsTheme();
    }
    
    private System.Threading.Timer? _themeMonitorTimer;
    private AppTheme _lastKnownWindowsTheme;
    
    public void StartMonitoringThemeChanges()
    {
        _lastKnownWindowsTheme = GetWindowsTheme();
        
        // Check every 2 seconds for theme changes
        _themeMonitorTimer = new System.Threading.Timer(_ =>
        {
            if (!IsFollowingSystem) return;
            
            var currentWindowsTheme = GetWindowsTheme();
            if (currentWindowsTheme != _lastKnownWindowsTheme)
            {
                _lastKnownWindowsTheme = currentWindowsTheme;
                Application.Current?.Dispatcher.Invoke(() =>
                {
                    ApplyTheme(currentWindowsTheme);
                });
            }
        }, null, TimeSpan.Zero, TimeSpan.FromSeconds(2));
    }
    
    public void StopMonitoringThemeChanges()
    {
        _themeMonitorTimer?.Dispose();
        _themeMonitorTimer = null;
    }
}
