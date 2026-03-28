using System.Collections.ObjectModel;
using System.Windows;
using System.Windows.Input;
using System.Windows.Media;
using JFStorageTester.Services;

namespace JFStorageTester.ViewModels;

public class SectorBlock : BaseViewModel
{
    private SolidColorBrush _color;

    public int Index { get; set; }
    public double ReadTimeMs { get; set; } // Store the read time for recalculation

    public SolidColorBrush Color
    {
        get => _color;
        set => SetProperty(ref _color, value);
    }

    public SectorBlock()
    {
        _color = new SolidColorBrush(System.Windows.Media.Color.FromRgb(60, 60, 60));
    }
}

public class SurfaceTestViewModel : BaseViewModel
{
    private readonly SurfaceTestService _testService;
    private System.Timers.Timer? _elapsedTimer;
    private DateTime _testStartTime;

    private bool _isRunning;
    private double _progressPercent;
    private double _averageSpeed;
    private long _totalSectors;
    private long _goodSectors;
    private long _badSectors;
    private long _slowSectors;
    private string _elapsedTime = "00:00:00";
    private string _remainingTime = "--:--:--";
    private int _gridColumns = 50;
    private const int TotalBlocks = 1000;

    // Dynamic heat map calibration
    private double _minReadTimeMs = double.MaxValue;
    private double _maxReadTimeMs = double.MinValue;
    private const double SlowThresholdMs = 200.0;   // Sectors taking >200ms are flagged as slow (yellow)
    private const double WarningTimeMs = 500.0;      // Very slow threshold

    // Result overlay
    private bool _showResultOverlay;
    private bool _testPassed;
    private bool _testHasWarnings;

    private static readonly SolidColorBrush BadColor = new(System.Windows.Media.Color.FromRgb(244, 67, 54));
    private static readonly SolidColorBrush PendingColor = new(System.Windows.Media.Color.FromRgb(60, 60, 60));
    private static readonly SolidColorBrush SlowColor = new(System.Windows.Media.Color.FromRgb(255, 193, 7)); // Yellow for slow sectors

    public SurfaceTestViewModel()
    {
        _testService = new SurfaceTestService();
        _testService.ProgressUpdated += OnProgressUpdated;
        _testService.TestCompleted += OnTestCompleted;
        _testService.ErrorOccurred += OnErrorOccurred;

        StartStopCommand = new RelayCommand(ToggleTest);

        TotalSectors = 0;
        GoodSectors = 0;
        BadSectors = 0;
        SlowSectors = 0;

        InitializeSectorBlocks();
    }

    public ObservableCollection<SectorBlock> SectorBlocks { get; } = new();

    public int GridColumns
    {
        get => _gridColumns;
        set => SetProperty(ref _gridColumns, value);
    }

    private void InitializeSectorBlocks()
    {
        SectorBlocks.Clear();
        for (int i = 0; i < TotalBlocks; i++)
        {
            SectorBlocks.Add(new SectorBlock
            {
                Index = i,
                Color = PendingColor,
                ReadTimeMs = -1
            });
        }
    }

    public void ResetBlocks()
    {
        _minReadTimeMs = double.MaxValue;
        _maxReadTimeMs = double.MinValue;
        foreach (var block in SectorBlocks)
        {
            block.Color = PendingColor;
            block.ReadTimeMs = -1;
        }
        
        // Reset stats and overlay
        ShowResultOverlay = false;
        ProgressPercent = 0;
        AverageSpeed = 0;
        TotalSectors = 0;
        GoodSectors = 0;
        BadSectors = 0;
        SlowSectors = 0;
        ElapsedTime = "00:00:00";
        RemainingTime = "--:--:--";
    }

    /// <summary>
    /// Called when the drive is physically removed during a test.
    /// </summary>
    public void StopTestFromDriveRemoval()
    {
        if (!IsRunning) return;
        
        _testService.StopTest();
        _elapsedTimer?.Stop();
        _elapsedTimer?.Dispose();
        _elapsedTimer = null;
        IsRunning = false;
        RemainingTime = "--:--:--";
        
        Application.Current?.Dispatcher.Invoke(() =>
        {
            MessageBox.Show("The drive was removed during the test. Test has been stopped.",
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

    public bool ShowResultOverlay
    {
        get => _showResultOverlay;
        set => SetProperty(ref _showResultOverlay, value);
    }

    public bool TestPassed
    {
        get => _testPassed;
        set => SetProperty(ref _testPassed, value);
    }

    public bool TestHasWarnings
    {
        get => _testHasWarnings;
        set => SetProperty(ref _testHasWarnings, value);
    }

    public double ProgressPercent
    {
        get => _progressPercent;
        set => SetProperty(ref _progressPercent, value);
    }

    public double AverageSpeed
    {
        get => _averageSpeed;
        set => SetProperty(ref _averageSpeed, value);
    }

    public long TotalSectors
    {
        get => _totalSectors;
        set => SetProperty(ref _totalSectors, value);
    }

    public long GoodSectors
    {
        get => _goodSectors;
        set => SetProperty(ref _goodSectors, value);
    }

    public long BadSectors
    {
        get => _badSectors;
        set => SetProperty(ref _badSectors, value);
    }

    public long SlowSectors
    {
        get => _slowSectors;
        set => SetProperty(ref _slowSectors, value);
    }

    public string ElapsedTime
    {
        get => _elapsedTime;
        set => SetProperty(ref _elapsedTime, value);
    }

    public string RemainingTime
    {
        get => _remainingTime;
        set => SetProperty(ref _remainingTime, value);
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
        var mainVm = Application.Current?.MainWindow?.DataContext as MainViewModel;
        var selectedDrive = mainVm?.SelectedDrive;

        if (selectedDrive == null)
        {
            MessageBox.Show("Please select a drive first.", "No Drive Selected",
                MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
        }

        IsRunning = true;
        ShowResultOverlay = false;

        ProgressPercent = 0;
        AverageSpeed = 0;
        GoodSectors = 0;
        BadSectors = 0;
        SlowSectors = 0;
        ElapsedTime = "00:00:00";
        RemainingTime = "--:--:--";
        ResetBlocks();

        _testStartTime = DateTime.Now;
        _elapsedTimer = new System.Timers.Timer(1000);
        _elapsedTimer.Elapsed += (s, e) => UpdateElapsedTime();
        _elapsedTimer.Start();

        // Use the physical drive path directly for full-disk scan
        await _testService.StartTestAsync(selectedDrive.DeviceId, TotalBlocks);
    }

    private void StopTest()
    {
        _testService.StopTest();
        _elapsedTimer?.Stop();
        _elapsedTimer?.Dispose();
        _elapsedTimer = null;
        IsRunning = false;
        RemainingTime = "--:--:--";
    }

    private void OnProgressUpdated(object? sender, SurfaceTestProgress progress)
    {
        Application.Current?.Dispatcher.Invoke(() =>
        {
            ProgressPercent = progress.ProgressPercent;
            TotalSectors = progress.TotalSectors;
            GoodSectors = progress.GoodSectors;
            BadSectors = progress.BadSectors;
            SlowSectors = progress.SlowSectors;
            AverageSpeed = progress.AverageSpeedMBps;

            if (progress.BlockIndex >= 0 && progress.BlockIndex < SectorBlocks.Count)
            {
                var block = SectorBlocks[progress.BlockIndex];
                block.ReadTimeMs = progress.BlockReadTimeMs;

                // Update min/max for calibration (only for good, non-slow blocks)
                if (progress.BlockIsGood && progress.BlockReadTimeMs > 0 && progress.BlockReadTimeMs < SlowThresholdMs)
                {
                    bool rangeChanged = false;
                    if (progress.BlockReadTimeMs < _minReadTimeMs)
                    {
                        _minReadTimeMs = progress.BlockReadTimeMs;
                        rangeChanged = true;
                    }
                    if (progress.BlockReadTimeMs > _maxReadTimeMs)
                    {
                        _maxReadTimeMs = progress.BlockReadTimeMs;
                        rangeChanged = true;
                    }

                    // Recalculate colors periodically when range changes
                    if (rangeChanged && progress.BlockIndex > 50 && progress.BlockIndex % 50 == 0)
                    {
                        RecalculateAllBlockColors();
                    }
                }

                // Set this block's color
                block.Color = GetHeatMapColor(progress.BlockIsGood, progress.BlockReadTimeMs);
            }

            if (progress.ProgressPercent > 0)
            {
                var elapsed = DateTime.Now - _testStartTime;
                var estimatedTotal = TimeSpan.FromMilliseconds(elapsed.TotalMilliseconds / progress.ProgressPercent * 100);
                var remaining = estimatedTotal - elapsed;
                if (remaining.TotalSeconds > 0)
                {
                    RemainingTime = remaining.ToString(@"hh\:mm\:ss");
                }
            }
        });
    }

    private void RecalculateAllBlockColors()
    {
        foreach (var block in SectorBlocks)
        {
            if (block.ReadTimeMs > 0)
            {
                block.Color = GetHeatMapColor(true, block.ReadTimeMs);
            }
        }
    }

    private SolidColorBrush GetHeatMapColor(bool isGood, double readTimeMs)
    {
        if (!isGood)
            return BadColor;

        // Flag slow sectors as yellow
        if (readTimeMs >= SlowThresholdMs)
            return SlowColor;

        // Use dynamic calibration based on observed min/max
        var minTime = _minReadTimeMs;
        var maxTime = _maxReadTimeMs;

        // Ensure we have valid range
        if (minTime >= maxTime || minTime == double.MaxValue)
        {
            return new SolidColorBrush(System.Windows.Media.Color.FromRgb(76, 175, 80));
        }

        var range = maxTime - minTime;
        if (range < 0.1) range = 0.1;

        var factor = (readTimeMs - minTime) / range;
        factor = Math.Max(0, Math.Min(1, factor));

        // Interpolate from bright green to dark green
        byte r = (byte)(76 - (61 * factor));
        byte g = (byte)(175 - (125 * factor));
        byte b = (byte)(80 - (62 * factor));

        return new SolidColorBrush(System.Windows.Media.Color.FromRgb(r, g, b));
    }

    private void OnTestCompleted(object? sender, EventArgs e)
    {
        Application.Current?.Dispatcher.Invoke(() =>
        {
            _elapsedTimer?.Stop();
            _elapsedTimer?.Dispose();
            _elapsedTimer = null;
            IsRunning = false;
            RemainingTime = "00:00:00";

            // Final recalculation of all colors with complete calibration data
            RecalculateAllBlockColors();

            if (ProgressPercent >= 99.9)
            {
                ProgressPercent = 100;

                // Show PASS/FAIL/WARNING overlay
                TestPassed = BadSectors == 0;
                TestHasWarnings = SlowSectors > 0 && BadSectors == 0;
                ShowResultOverlay = true;
            }
        });
    }

    private void OnErrorOccurred(object? sender, string errorMessage)
    {
        Application.Current?.Dispatcher.Invoke(() =>
        {
            StopTest();
            MessageBox.Show($"Error during surface test:\n\n{errorMessage}\n\nTry running as Administrator.",
                "Test Error", MessageBoxButton.OK, MessageBoxImage.Error);
        });
    }

    private void UpdateElapsedTime()
    {
        var elapsed = DateTime.Now - _testStartTime;
        Application.Current?.Dispatcher.Invoke(() =>
        {
            ElapsedTime = elapsed.ToString(@"hh\:mm\:ss");
        });
    }
}
