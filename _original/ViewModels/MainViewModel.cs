using System.Collections.ObjectModel;
using System.Windows;
using System.Windows.Input;
using JFStorageTester.Models;
using JFStorageTester.Services;

namespace JFStorageTester.ViewModels;

public class MainViewModel : BaseViewModel
{
    private readonly DriveDetectionService _driveService;
    private readonly ThemeService _themeService;
    
    private PhysicalDiskModel? _selectedDrive;
    private PartitionInfoModel? _selectedPartition;
    private int _selectedTabIndex = 0;
    private bool _isTestRunning;
    
    // Child ViewModels
    public SurfaceTestViewModel SurfaceTestViewModel { get; }
    public SpeedTestViewModel SpeedTestViewModel { get; }
    
    public MainViewModel()
    {
        _driveService = DriveDetectionService.Instance;
        _themeService = ThemeService.Instance;
        
        // Initialize child ViewModels
        SurfaceTestViewModel = new SurfaceTestViewModel();
        SpeedTestViewModel = new SpeedTestViewModel();
        
        Drives = _driveService.Drives;
        
        // Select first drive by default
        if (Drives.Count > 0)
        {
            SelectedDrive = Drives[0];
        }
        
        // Subscribe to drive changes
        _driveService.DrivesChanged += (s, e) =>
        {
            Application.Current?.Dispatcher.Invoke(() =>
            {
                OnPropertyChanged(nameof(Drives));
                
                if (IsTestRunning)
                {
                    // Don't change selection during tests, but check if our drive was removed
                    if (SelectedDrive != null)
                    {
                        var stillExists = Drives.Any(d => d.DiskNumber == SelectedDrive.DiskNumber);
                        if (!stillExists)
                        {
                            // Drive was removed during test - stop the test
                            SurfaceTestViewModel.StopTestFromDriveRemoval();
                            SpeedTestViewModel.StopTestFromDriveRemoval();
                        }
                    }
                    return;
                }
                
                // Reselect drive if it still exists, otherwise select first
                if (SelectedDrive != null)
                {
                    var existingDrive = Drives.FirstOrDefault(d => d.DiskNumber == SelectedDrive.DiskNumber);
                    if (existingDrive != null)
                    {
                        SelectedDrive = existingDrive;
                    }
                    else if (Drives.Count > 0)
                    {
                        SelectedDrive = Drives[0];
                    }
                }
                else if (Drives.Count > 0)
                {
                    SelectedDrive = Drives[0];
                }
            });
        };
        
        // Commands
        OpenSettingsCommand = new RelayCommand(OpenSettings);
        RefreshDrivesCommand = new RelayCommand(RefreshDrives);
        OpenDiskManagementCommand = new RelayCommand(OpenDiskManagement);
        OpenDiskPartCommand = new RelayCommand(OpenDiskPart);
        
        // Subscribe to theme changes to update UI
        _themeService.ThemeChanged += (s, e) => OnPropertyChanged(nameof(CurrentTheme));
    }
    
    public ObservableCollection<PhysicalDiskModel> Drives { get; }
    
    public PhysicalDiskModel? SelectedDrive
    {
        get => _selectedDrive;
        set
        {
            var previousDrive = _selectedDrive;
            if (SetProperty(ref _selectedDrive, value))
            {
                OnPropertyChanged(nameof(IsDriveSelected));
                OnPropertyChanged(nameof(CanUseSpeedTest));
                OnPropertyChanged(nameof(SpeedTestLockedMessage));
                OnPropertyChanged(nameof(Partitions));
                
                // Push partition data to SpeedTestViewModel
                SpeedTestViewModel.Partitions = value?.Partitions;
                
                // Auto-select best partition for speed test
                if (value != null)
                {
                    SelectedPartition = value.BestSpeedTestPartition;
                }
                else
                {
                    SelectedPartition = null;
                }
                
                // Reset test results when drive changes
                if (previousDrive != null && value != null && previousDrive.DiskNumber != value.DiskNumber)
                {
                    SurfaceTestViewModel.ResetBlocks();
                    SpeedTestViewModel.ResetResults();
                }
            }
        }
    }
    
    /// <summary>
    /// The currently selected partition for speed testing.
    /// </summary>
    public PartitionInfoModel? SelectedPartition
    {
        get => _selectedPartition;
        set
        {
            if (SetProperty(ref _selectedPartition, value))
            {
                OnPropertyChanged(nameof(CanUseSpeedTest));
                OnPropertyChanged(nameof(SpeedTestLockedMessage));
                
                // Sync to SpeedTestViewModel
                SpeedTestViewModel.SelectedPartition = value;
            }
        }
    }
    
    /// <summary>
    /// Partitions of the currently selected physical disk.
    /// </summary>
    public List<PartitionInfoModel>? Partitions => SelectedDrive?.Partitions;
    
    public int SelectedTabIndex
    {
        get => _selectedTabIndex;
        set => SetProperty(ref _selectedTabIndex, value);
    }
    
    public bool IsTestRunning
    {
        get => _isTestRunning;
        set
        {
            if (SetProperty(ref _isTestRunning, value))
            {
                OnPropertyChanged(nameof(CanChangeDrive));
                OnPropertyChanged(nameof(CanChangeTab));
                OnPropertyChanged(nameof(CanRefreshDrives));
            }
        }
    }
    
    public bool IsDriveSelected => SelectedDrive != null;
    
    public bool CanChangeDrive => !IsTestRunning;
    
    public bool CanChangeTab => !IsTestRunning;
    
    public bool CanRefreshDrives => !IsTestRunning;
    
    public bool CanUseSpeedTest => SelectedDrive != null && SelectedPartition != null 
        && !SelectedPartition.IsBitLockerLocked;
    
    public string SpeedTestLockedMessage
    {
        get
        {
            if (SelectedDrive == null) return "";
            if (SelectedDrive.Partitions.Count == 0) return "No partitions found on this disk";
            if (SelectedPartition?.IsBitLockerLocked == true) return "Speed test unavailable: Partition is BitLocker locked";
            if (SelectedDrive.HasBitLockerLocked) return "Some partitions are BitLocker locked";
            return "";
        }
    }
    
    public AppTheme CurrentTheme => _themeService.CurrentTheme;
    
    // Commands
    public ICommand OpenSettingsCommand { get; }
    public ICommand RefreshDrivesCommand { get; }
    public ICommand OpenDiskManagementCommand { get; }
    public ICommand OpenDiskPartCommand { get; }
    
    private void OpenSettings()
    {
        var settingsWindow = new Views.SettingsWindow
        {
            Owner = Application.Current.MainWindow
        };
        settingsWindow.ShowDialog();
    }
    
    private void RefreshDrives()
    {
        _driveService.RefreshDrives();
    }
    
    private void OpenDiskManagement()
    {
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = "diskmgmt.msc",
                UseShellExecute = true
            });
        }
        catch { }
    }
    
    private void OpenDiskPart()
    {
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = "cmd.exe",
                Arguments = "/k diskpart",
                UseShellExecute = true
            });
        }
        catch { }
    }
}
