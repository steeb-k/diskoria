using System.Collections.Generic;
using System.Windows;
using System.Windows.Input;
using JFStorageTester.Models;
using JFStorageTester.Services;

namespace JFStorageTester.ViewModels;

public class SpeedTestViewModel : BaseViewModel
{
    private readonly SpeedTestService _testService;

    private bool _isRunning;
    private bool _includeWriteTests = true;
    private double _progressPercent;
    private string _currentOperation = "Ready";
    private double _currentSpeed;
    private List<PartitionInfoModel>? _partitions;
    private PartitionInfoModel? _selectedPartition;

    // Results
    private double _sequentialReadSpeed = -1;
    private double _sequentialWriteSpeed = -1;
    private double _random4KReadSpeed = -1;
    private double _random4KWriteSpeed = -1;

    public SpeedTestViewModel()
    {
        _testService = new SpeedTestService();
        _testService.ProgressUpdated += OnProgressUpdated;
        _testService.TestCompleted += OnTestCompleted;

        StartStopCommand = new RelayCommand(ToggleTest);
    }

    /// <summary>
    /// Available partitions on the selected physical disk (set by MainViewModel).
    /// </summary>
    public List<PartitionInfoModel>? Partitions
    {
        get => _partitions;
        set => SetProperty(ref _partitions, value);
    }

    /// <summary>
    /// Currently selected partition for speed testing (set by MainViewModel).
    /// </summary>
    public PartitionInfoModel? SelectedPartition
    {
        get => _selectedPartition;
        set => SetProperty(ref _selectedPartition, value);
    }

    public void ResetResults()
    {
        SequentialReadSpeed = -1;
        SequentialWriteSpeed = -1;
        Random4KReadSpeed = -1;
        Random4KWriteSpeed = -1;
        ProgressPercent = 0;
        CurrentOperation = "Ready";
        CurrentSpeed = 0;
    }

    /// <summary>
    /// Called when the drive is physically removed during a test.
    /// </summary>
    public void StopTestFromDriveRemoval()
    {
        if (!IsRunning) return;
        
        _testService.StopTest();
        IsRunning = false;
        CurrentOperation = "Drive Removed";
        
        Application.Current?.Dispatcher.Invoke(() =>
        {
            MessageBox.Show("The drive was removed during the speed test. Test has been stopped.",
                "Drive Removed", MessageBoxButton.OK, MessageBoxImage.Warning);
        });
    }

    public bool IsRunning
    {
        get => _isRunning;
        set
        {
            if (SetProperty(ref _isRunning, value))
            {
                OnPropertyChanged(nameof(IsNotRunning));
                OnPropertyChanged(nameof(ButtonText));

                if (Application.Current?.MainWindow?.DataContext is MainViewModel mainVm)
                {
                    mainVm.IsTestRunning = value;
                }
            }
        }
    }

    public bool IsNotRunning => !IsRunning;

    public string ButtonText => IsRunning ? "Stop Test" : "Start Test";

    public bool IncludeWriteTests
    {
        get => _includeWriteTests;
        set
        {
            if (SetProperty(ref _includeWriteTests, value))
            {
                OnPropertyChanged(nameof(WriteTestsVisibility));
            }
        }
    }

    public Visibility WriteTestsVisibility => IncludeWriteTests ? Visibility.Visible : Visibility.Collapsed;

    public double ProgressPercent
    {
        get => _progressPercent;
        set => SetProperty(ref _progressPercent, value);
    }

    public string CurrentOperation
    {
        get => _currentOperation;
        set => SetProperty(ref _currentOperation, value);
    }

    public double CurrentSpeed
    {
        get => _currentSpeed;
        set => SetProperty(ref _currentSpeed, value);
    }

    // Results with formatted display
    public double SequentialReadSpeed
    {
        get => _sequentialReadSpeed;
        set
        {
            if (SetProperty(ref _sequentialReadSpeed, value))
                OnPropertyChanged(nameof(SequentialReadDisplay));
        }
    }

    public double SequentialWriteSpeed
    {
        get => _sequentialWriteSpeed;
        set
        {
            if (SetProperty(ref _sequentialWriteSpeed, value))
                OnPropertyChanged(nameof(SequentialWriteDisplay));
        }
    }

    public double Random4KReadSpeed
    {
        get => _random4KReadSpeed;
        set
        {
            if (SetProperty(ref _random4KReadSpeed, value))
                OnPropertyChanged(nameof(Random4KReadDisplay));
        }
    }

    public double Random4KWriteSpeed
    {
        get => _random4KWriteSpeed;
        set
        {
            if (SetProperty(ref _random4KWriteSpeed, value))
                OnPropertyChanged(nameof(Random4KWriteDisplay));
        }
    }

    public string SequentialReadDisplay => FormatSpeed(_sequentialReadSpeed);
    public string SequentialWriteDisplay => FormatSpeed(_sequentialWriteSpeed);
    public string Random4KReadDisplay => FormatSpeed(_random4KReadSpeed);
    public string Random4KWriteDisplay => FormatSpeed(_random4KWriteSpeed);

    private static string FormatSpeed(double speedMBps)
    {
        if (speedMBps < 0) return "---";
        if (speedMBps >= 1000) return $"{speedMBps:F0}";
        if (speedMBps >= 100) return $"{speedMBps:F1}";
        return $"{speedMBps:F2}";
    }

    public ICommand StartStopCommand { get; }

    private void ToggleTest()
    {
        if (IsRunning)
        {
            StopTest();
        }
        else
        {
            StartTest();
        }
    }

    private async void StartTest()
    {
        var selectedPartition = SelectedPartition;

        if (selectedPartition == null)
        {
            MessageBox.Show("Please select a partition for the speed test.", "No Partition Selected",
                MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
        }

        // Warn about write tests
        // Determine test file size for display
        var mainVm2 = Application.Current?.MainWindow?.DataContext as MainViewModel;
        var driveTypeForSize = mainVm2?.SelectedDrive?.DriveType ?? Models.StorageType.Unknown;
        var testFileSize = (driveTypeForSize is Models.StorageType.NVMe or Models.StorageType.SSD or Models.StorageType.HDD) ? "1 GB" : "256 MB";

        if (IncludeWriteTests)
        {
            var result = MessageBox.Show(
                $"Write tests will create a temporary {testFileSize} file on {selectedPartition.DriveLetter}\n\n" +
                "The file will be automatically deleted after the test.\n\n" +
                "Continue with write tests?",
                "Write Test Confirmation",
                MessageBoxButton.YesNo,
                MessageBoxImage.Question);

            if (result != MessageBoxResult.Yes)
                return;
        }

        IsRunning = true;

        // Reset results
        SequentialReadSpeed = -1;
        SequentialWriteSpeed = -1;
        Random4KReadSpeed = -1;
        Random4KWriteSpeed = -1;
        ProgressPercent = 0;
        CurrentOperation = "Starting...";
        CurrentSpeed = 0;

        // Get the drive type from MainViewModel for dynamic test sizing
        var mainVm = Application.Current?.MainWindow?.DataContext as MainViewModel;
        var driveType = mainVm?.SelectedDrive?.DriveType ?? Models.StorageType.Unknown;

        await _testService.StartTestAsync(selectedPartition.DriveLetter, IncludeWriteTests, driveType);
    }

    private void StopTest()
    {
        _testService.StopTest();
        IsRunning = false;
        CurrentOperation = "Stopped";
    }

    private void OnProgressUpdated(object? sender, SpeedTestProgress progress)
    {
        Application.Current?.Dispatcher.Invoke(() =>
        {
            ProgressPercent = progress.ProgressPercent;
            CurrentOperation = progress.CurrentOperation;
            CurrentSpeed = progress.CurrentSpeedMBps;
        });
    }

    private void OnTestCompleted(object? sender, SpeedTestResult result)
    {
        Application.Current?.Dispatcher.Invoke(() =>
        {
            IsRunning = false;

            if (result.Completed)
            {
                SequentialReadSpeed = result.SequentialReadMBps;
                SequentialWriteSpeed = result.SequentialWriteMBps;
                Random4KReadSpeed = result.Random4KReadMBps;
                Random4KWriteSpeed = result.Random4KWriteMBps;
                CurrentOperation = "Complete";
                ProgressPercent = 100;
            }
            else if (!string.IsNullOrEmpty(result.ErrorMessage))
            {
                CurrentOperation = "Error";
                MessageBox.Show(
                    $"Speed test failed:\n\n{result.ErrorMessage}",
                    "Test Error",
                    MessageBoxButton.OK,
                    MessageBoxImage.Error);
            }
        });
    }
}
