using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using JFStorageTester.Services;

namespace JFStorageTester.Views;

public partial class SettingsWindow : Window
{
    [DllImport("dwmapi.dll", PreserveSig = true)]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int attrValue, int attrSize);

    private const int DWMWA_USE_IMMERSIVE_DARK_MODE = 20;

    public SettingsWindow()
    {
        InitializeComponent();
        
        Loaded += SettingsWindow_Loaded;
        ThemeService.Instance.ThemeChanged += OnThemeChanged;
    }

    private void SettingsWindow_Loaded(object sender, RoutedEventArgs e)
    {
        ApplyTitleBarTheme(ThemeService.Instance.CurrentTheme);
    }

    private void OnThemeChanged(object? sender, AppTheme theme)
    {
        ApplyTitleBarTheme(theme);
    }

    private void ApplyTitleBarTheme(AppTheme theme)
    {
        var hwnd = new WindowInteropHelper(this).Handle;
        if (hwnd == IntPtr.Zero) return;

        int useDarkMode = theme == AppTheme.Dark ? 1 : 0;
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, ref useDarkMode, sizeof(int));
    }

    private void CloseButton_Click(object sender, RoutedEventArgs e)
    {
        Close();
    }

    protected override void OnClosed(EventArgs e)
    {
        ThemeService.Instance.ThemeChanged -= OnThemeChanged;
        base.OnClosed(e);
    }
}
