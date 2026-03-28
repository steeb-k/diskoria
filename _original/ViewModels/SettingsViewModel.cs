using System.Windows;
using System.Windows.Input;
using JFStorageTester.Services;

namespace JFStorageTester.ViewModels;

public class SettingsViewModel : BaseViewModel
{
    private readonly ThemeService _themeService;
    private readonly UpdateService _updateService;

    public SettingsViewModel()
    {
        _themeService = ThemeService.Instance;
        _updateService = new UpdateService();
        
        _updateService.DownloadProgressChanged += (s, progress) =>
        {
            DownloadProgress = progress;
        };

        // Initialize from current state
        _isDarkMode = _themeService.CurrentTheme == AppTheme.Dark;
        _isFollowingSystem = _themeService.IsFollowingSystem;
        _currentVersion = UpdateService.CurrentVersion;

        // Subscribe to theme changes
        _themeService.ThemeChanged += (s, theme) =>
        {
            _isDarkMode = theme == AppTheme.Dark;
            OnPropertyChanged(nameof(IsDarkMode));
        };

        // Commands
        ToggleThemeCommand = new RelayCommand(ToggleTheme);
        FollowSystemThemeCommand = new RelayCommand(FollowSystemTheme);
        CheckForUpdatesCommand = new RelayCommand(async () => await CheckForUpdatesAsync(), () => !IsCheckingForUpdates && !IsDownloading);
        InstallUpdateCommand = new RelayCommand(async () => await InstallUpdateAsync(), () => UpdateAvailable && !IsDownloading);
    }

    private bool _isDarkMode;
    public bool IsDarkMode
    {
        get => _isDarkMode;
        set
        {
            if (SetProperty(ref _isDarkMode, value))
            {
                IsFollowingSystem = false;
                _themeService.SetTheme(value ? AppTheme.Dark : AppTheme.Light);
            }
        }
    }

    private bool _isFollowingSystem;
    public bool IsFollowingSystem
    {
        get => _isFollowingSystem;
        set
        {
            if (SetProperty(ref _isFollowingSystem, value))
            {
                _themeService.IsFollowingSystem = value;
                if (value)
                {
                    _themeService.FollowSystemTheme();
                }
            }
        }
    }
    
    private string _currentVersion = "1.0.0";
    public string CurrentVersion
    {
        get => _currentVersion;
        set => SetProperty(ref _currentVersion, value);
    }
    
    private bool _isCheckingForUpdates;
    public bool IsCheckingForUpdates
    {
        get => _isCheckingForUpdates;
        set => SetProperty(ref _isCheckingForUpdates, value);
    }
    
    private bool _updateAvailable;
    public bool UpdateAvailable
    {
        get => _updateAvailable;
        set
        {
            if (SetProperty(ref _updateAvailable, value))
            {
                (InstallUpdateCommand as RelayCommand)?.RaiseCanExecuteChanged();
            }
        }
    }
    
    private string _latestVersion = "";
    public string LatestVersion
    {
        get => _latestVersion;
        set => SetProperty(ref _latestVersion, value);
    }
    
    private string _updateStatus = "";
    public string UpdateStatus
    {
        get => _updateStatus;
        set => SetProperty(ref _updateStatus, value);
    }
    
    private bool _isDownloading;
    public bool IsDownloading
    {
        get => _isDownloading;
        set => SetProperty(ref _isDownloading, value);
    }
    
    private double _downloadProgress;
    public double DownloadProgress
    {
        get => _downloadProgress;
        set => SetProperty(ref _downloadProgress, value);
    }
    
    private string _downloadUrl = "";

    public ICommand ToggleThemeCommand { get; }
    public ICommand FollowSystemThemeCommand { get; }
    public ICommand CheckForUpdatesCommand { get; }
    public ICommand InstallUpdateCommand { get; }

    private void ToggleTheme()
    {
        IsDarkMode = !IsDarkMode;
    }

    private void FollowSystemTheme()
    {
        IsFollowingSystem = true;
    }
    
    private async Task CheckForUpdatesAsync()
    {
        IsCheckingForUpdates = true;
        UpdateStatus = "Checking for updates...";
        UpdateAvailable = false;
        
        try
        {
            var result = await _updateService.CheckForUpdatesAsync();
            
            if (result.ErrorMessage != null)
            {
                UpdateStatus = result.ErrorMessage;
            }
            else if (result.UpdateAvailable)
            {
                UpdateAvailable = true;
                LatestVersion = result.LatestVersion;
                _downloadUrl = result.DownloadUrl;
                UpdateStatus = $"Update available: v{result.LatestVersion}";
            }
            else
            {
                UpdateStatus = "You're running the latest version!";
            }
        }
        catch (Exception ex)
        {
            UpdateStatus = $"Error: {ex.Message}";
        }
        finally
        {
            IsCheckingForUpdates = false;
        }
    }
    
    private async Task InstallUpdateAsync()
    {
        if (string.IsNullOrEmpty(_downloadUrl))
        {
            UpdateStatus = "No download URL available";
            return;
        }
        
        var result = MessageBox.Show(
            $"Download and install version {LatestVersion}?\n\nThe application will restart after the update.",
            "Install Update",
            MessageBoxButton.YesNo,
            MessageBoxImage.Question);
        
        if (result != MessageBoxResult.Yes) return;
        
        IsDownloading = true;
        DownloadProgress = 0;
        UpdateStatus = "Downloading update...";
        
        try
        {
            var success = await _updateService.DownloadAndInstallUpdateAsync(_downloadUrl);
            
            if (!success)
            {
                UpdateStatus = "Update failed. Please try again.";
                IsDownloading = false;
            }
            // If successful, the app will close and restart
        }
        catch (Exception ex)
        {
            UpdateStatus = $"Error: {ex.Message}";
            IsDownloading = false;
        }
    }
}
