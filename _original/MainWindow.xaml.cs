using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media.Imaging;
using JFStorageTester.Services;

namespace JFStorageTester;

public partial class MainWindow : Window
{
    [DllImport("dwmapi.dll", PreserveSig = true)]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int attrValue, int attrSize);

    private const int DWMWA_USE_IMMERSIVE_DARK_MODE = 20;

    public MainWindow()
    {
        InitializeComponent();

        Loaded += MainWindow_Loaded;
        ThemeService.Instance.ThemeChanged += OnThemeChanged;
    }

    private void MainWindow_Loaded(object sender, RoutedEventArgs e)
    {
        ApplyTheme(ThemeService.Instance.CurrentTheme);
    }

    private void OnThemeChanged(object? sender, AppTheme theme)
    {
        ApplyTheme(theme);
    }

    private void ApplyTheme(AppTheme theme)
    {
        ApplyTitleBarTheme(theme);
        ApplyIcon(theme);
    }

    private void ApplyTitleBarTheme(AppTheme theme)
    {
        var hwnd = new WindowInteropHelper(this).Handle;
        if (hwnd == IntPtr.Zero) return;

        int useDarkMode = theme == AppTheme.Dark ? 1 : 0;
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, ref useDarkMode, sizeof(int));
    }

    private void ApplyIcon(AppTheme theme)
    {
        var iconPath = theme == AppTheme.Dark
            ? "pack://application:,,,/JF - Black.ico"
            : "pack://application:,,,/JF - White.ico";

        var decoder = BitmapDecoder.Create(
            new Uri(iconPath, UriKind.Absolute),
            BitmapCreateOptions.PreservePixelFormat,
            BitmapCacheOption.OnLoad);

        // Find the largest frame for best quality
        var largestFrame = decoder.Frames
            .OrderByDescending(f => f.PixelWidth)
            .First();

        Icon = largestFrame;
    }

    protected override void OnClosed(EventArgs e)
    {
        ThemeService.Instance.ThemeChanged -= OnThemeChanged;
        base.OnClosed(e);
    }
}
