using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace JFStorageTester.Services;

public class SpeedTestResult
{
    public double SequentialReadMBps { get; set; }
    public double SequentialWriteMBps { get; set; }
    public double Random4KReadMBps { get; set; }
    public double Random4KWriteMBps { get; set; }
    public bool Completed { get; set; }
    public string? ErrorMessage { get; set; }
}

public class SpeedTestProgress
{
    public string CurrentOperation { get; set; } = "";
    public double ProgressPercent { get; set; }
    public double CurrentSpeedMBps { get; set; }
}

public class SpeedTestService
{
    private const int SequentialBlockSize = 1024 * 1024; // 1 MB blocks for sequential
    private const int Random4KBlockSize = 4096; // 4 KB blocks for random

    // Dynamic test parameters based on drive type
    private int _testFileSizeMB = 1024;
    private int _random4KIterations = 4096;
    private int _testPasses = 3;

    private void ConfigureForDriveType(Models.StorageType driveType)
    {
        switch (driveType)
        {
            case Models.StorageType.NVMe:
            case Models.StorageType.SSD:
                _testFileSizeMB = 1024;       // 1 GB
                _random4KIterations = 4096;
                _testPasses = 3;
                break;
            case Models.StorageType.HDD:
                _testFileSizeMB = 1024;       // 1 GB
                _random4KIterations = 2048;
                _testPasses = 3;
                break;
            case Models.StorageType.USB:
            case Models.StorageType.eMMC:
            default:
                _testFileSizeMB = 256;        // 256 MB
                _random4KIterations = 1024;
                _testPasses = 2;
                break;
        }
    }

    // P/Invoke for unbuffered I/O
    private const uint GENERIC_READ = 0x80000000;
    private const uint GENERIC_WRITE = 0x40000000;
    private const uint FILE_SHARE_READ = 0x00000001;
    private const uint FILE_SHARE_WRITE = 0x00000002;
    private const uint CREATE_ALWAYS = 2;
    private const uint OPEN_EXISTING = 3;
    private const uint FILE_FLAG_NO_BUFFERING = 0x20000000;
    private const uint FILE_FLAG_WRITE_THROUGH = 0x80000000;
    private const uint FILE_FLAG_SEQUENTIAL_SCAN = 0x08000000;

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern SafeFileHandle CreateFile(
        string lpFileName,
        uint dwDesiredAccess,
        uint dwShareMode,
        IntPtr lpSecurityAttributes,
        uint dwCreationDisposition,
        uint dwFlagsAndAttributes,
        IntPtr hTemplateFile);

    private CancellationTokenSource? _cts;
    private bool _isRunning;

    public event EventHandler<SpeedTestProgress>? ProgressUpdated;
    public event EventHandler<SpeedTestResult>? TestCompleted;

    public bool IsRunning => _isRunning;

    public async Task StartTestAsync(string driveLetter, bool includeWriteTests, Models.StorageType driveType = Models.StorageType.Unknown)
    {
        if (_isRunning) return;

        _isRunning = true;
        _cts = new CancellationTokenSource();
        ConfigureForDriveType(driveType);

        var result = new SpeedTestResult();
        string? testFilePath = null;

        try
        {
            // Create test file path - normalize drive letter to "X:\" format
            var cleanDrive = driveLetter.TrimEnd(':', '\\');
            var drivePath = cleanDrive + ":\\";
            testFilePath = Path.Combine(drivePath, $"JFStorageTester_SpeedTest_{Guid.NewGuid():N}.tmp");

            // Ensure drive is ready
            var driveInfo = new DriveInfo(cleanDrive);
            if (!driveInfo.IsReady)
            {
                throw new IOException("Drive is not ready");
            }

            // Check available space - need 2x test file size for safety
            long requiredSpace = (long)_testFileSizeMB * 1024 * 1024 * 2;
            if (driveInfo.AvailableFreeSpace < requiredSpace)
            {
                throw new IOException($"Not enough free space. Need at least {_testFileSizeMB * 2} MB free.");
            }

            if (includeWriteTests)
            {
                // Sequential Write Test (with warmup + multiple passes)
                ReportProgress("Sequential Write", 0, 0);
                result.SequentialWriteMBps = await Task.Run(() => RunSequentialWriteTestMultiPass(testFilePath, _cts.Token));

                if (_cts.Token.IsCancellationRequested) return;

                // Sequential Read Test (with multiple passes)
                ReportProgress("Sequential Read", 25, 0);
                result.SequentialReadMBps = await Task.Run(() => RunSequentialReadTestMultiPass(testFilePath, _cts.Token));

                if (_cts.Token.IsCancellationRequested) return;

                // Random 4K Write Test
                ReportProgress("Random 4K Write", 50, 0);
                result.Random4KWriteMBps = await Task.Run(() => RunRandom4KWriteTestMultiPass(testFilePath, _cts.Token));

                if (_cts.Token.IsCancellationRequested) return;

                // Random 4K Read Test
                ReportProgress("Random 4K Read", 75, 0);
                result.Random4KReadMBps = await Task.Run(() => RunRandom4KReadTestMultiPass(testFilePath, _cts.Token));
            }
            else
            {
                // Read-only mode - create a temp file first
                ReportProgress("Preparing test file...", 0, 0);
                await Task.Run(() => CreateTestFile(testFilePath, _cts.Token));

                if (_cts.Token.IsCancellationRequested) return;

                // Sequential Read Test
                ReportProgress("Sequential Read", 25, 0);
                result.SequentialReadMBps = await Task.Run(() => RunSequentialReadTestMultiPass(testFilePath, _cts.Token));

                if (_cts.Token.IsCancellationRequested) return;

                // Random 4K Read Test
                ReportProgress("Random 4K Read", 75, 0);
                result.Random4KReadMBps = await Task.Run(() => RunRandom4KReadTestMultiPass(testFilePath, _cts.Token));

                // Mark write tests as N/A
                result.SequentialWriteMBps = -1;
                result.Random4KWriteMBps = -1;
            }

            result.Completed = true;
        }
        catch (OperationCanceledException)
        {
            result.Completed = false;
        }
        catch (Exception ex)
        {
            result.Completed = false;
            result.ErrorMessage = ex.Message;
        }
        finally
        {
            // Clean up test file
            if (testFilePath != null)
            {
                try
                {
                    if (File.Exists(testFilePath))
                    {
                        File.Delete(testFilePath);
                    }
                }
                catch
                {
                    // Ignore cleanup errors
                }
            }

            _isRunning = false;
            ReportProgress("Complete", 100, 0);
            TestCompleted?.Invoke(this, result);
        }
    }

    public void StopTest()
    {
        _cts?.Cancel();
    }

    private void CreateTestFile(string path, CancellationToken ct)
    {
        // Use buffered write with random data for creating the file
        var buffer = new byte[SequentialBlockSize];
        var rng = new Random(42); // Fixed seed for reproducibility
        
        using var fs = new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.None,
            SequentialBlockSize, FileOptions.WriteThrough);

        int totalBlocks = _testFileSizeMB;
        for (int i = 0; i < totalBlocks; i++)
        {
            ct.ThrowIfCancellationRequested();
            rng.NextBytes(buffer);
            fs.Write(buffer, 0, buffer.Length);
            ReportProgress("Preparing test file...", (double)i / totalBlocks * 20, 0);
        }

        fs.Flush();
    }

    private double RunSequentialWriteTestMultiPass(string path, CancellationToken ct)
    {
        var results = new List<double>();
        
        for (int pass = 0; pass < _testPasses; pass++)
        {
            ct.ThrowIfCancellationRequested();
            
            var speed = RunSequentialWriteTest(path, ct, pass);
            results.Add(speed);
            
            // Small delay between passes to let drive settle
            if (pass < _testPasses - 1)
                Thread.Sleep(100);
        }
        
        // Return average of all passes (excluding warmup if we had one)
        return results.Average();
    }

    private double RunSequentialWriteTest(string path, CancellationToken ct, int passNumber)
    {
        // Buffer must be aligned for unbuffered I/O
        var buffer = new byte[SequentialBlockSize];
        var rng = new Random();
        rng.NextBytes(buffer);

        // Use unbuffered I/O for accurate write speed
        using var handle = CreateFile(
            path,
            GENERIC_WRITE,
            0,
            IntPtr.Zero,
            CREATE_ALWAYS,
            FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH | FILE_FLAG_SEQUENTIAL_SCAN,
            IntPtr.Zero);

        if (handle.IsInvalid)
        {
            throw new IOException($"Failed to open file for writing. Error: {Marshal.GetLastWin32Error()}");
        }

        using var fs = new FileStream(handle, FileAccess.Write, SequentialBlockSize, false);

        var sw = Stopwatch.StartNew();
        long totalBytesWritten = 0;

        int totalBlocks = _testFileSizeMB;
        for (int i = 0; i < totalBlocks; i++)
        {
            ct.ThrowIfCancellationRequested();

            fs.Write(buffer, 0, buffer.Length);
            totalBytesWritten += buffer.Length;

            if (i % 32 == 0) // Update less frequently for better performance
            {
                var elapsed = sw.Elapsed.TotalSeconds;
                var currentSpeed = elapsed > 0 ? totalBytesWritten / elapsed / (1024 * 1024) : 0;
                var passProgress = (double)i / totalBlocks;
                var overallProgress = ((double)passNumber + passProgress) / _testPasses * 25;
                ReportProgress($"Sequential Write (Pass {passNumber + 1}/{_testPasses})", overallProgress, currentSpeed);
            }
        }

        fs.Flush();
        sw.Stop();

        var totalSeconds = sw.Elapsed.TotalSeconds;
        return totalSeconds > 0 ? totalBytesWritten / totalSeconds / (1024 * 1024) : 0;
    }

    private double RunSequentialReadTestMultiPass(string path, CancellationToken ct)
    {
        var results = new List<double>();
        
        for (int pass = 0; pass < _testPasses; pass++)
        {
            ct.ThrowIfCancellationRequested();
            
            var speed = RunSequentialReadTest(path, ct, pass);
            results.Add(speed);
            
            // Small delay between passes
            if (pass < _testPasses - 1)
                Thread.Sleep(100);
        }
        
        return results.Average();
    }

    private double RunSequentialReadTest(string path, CancellationToken ct, int passNumber)
    {
        // Buffer must be aligned for unbuffered I/O
        var buffer = new byte[SequentialBlockSize];

        // Use unbuffered I/O to bypass Windows cache - this gives TRUE read speed
        using var handle = CreateFile(
            path,
            GENERIC_READ,
            FILE_SHARE_READ,
            IntPtr.Zero,
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN,
            IntPtr.Zero);

        if (handle.IsInvalid)
        {
            throw new IOException($"Failed to open file for reading. Error: {Marshal.GetLastWin32Error()}");
        }

        using var fs = new FileStream(handle, FileAccess.Read, SequentialBlockSize, false);

        var fileSize = new FileInfo(path).Length;
        var sw = Stopwatch.StartNew();
        long totalBytesRead = 0;

        while (totalBytesRead < fileSize)
        {
            ct.ThrowIfCancellationRequested();

            int bytesRead = fs.Read(buffer, 0, buffer.Length);
            if (bytesRead == 0) break;

            totalBytesRead += bytesRead;

            if (totalBytesRead % (32 * SequentialBlockSize) == 0)
            {
                var elapsed = sw.Elapsed.TotalSeconds;
                var currentSpeed = elapsed > 0 ? totalBytesRead / elapsed / (1024 * 1024) : 0;
                var passProgress = (double)totalBytesRead / fileSize;
                var overallProgress = 25 + ((double)passNumber + passProgress) / _testPasses * 25;
                ReportProgress($"Sequential Read (Pass {passNumber + 1}/{_testPasses})", overallProgress, currentSpeed);
            }
        }

        sw.Stop();

        var totalSeconds = sw.Elapsed.TotalSeconds;
        return totalSeconds > 0 ? totalBytesRead / totalSeconds / (1024 * 1024) : 0;
    }

    private double RunRandom4KWriteTestMultiPass(string path, CancellationToken ct)
    {
        var results = new List<double>();
        
        for (int pass = 0; pass < _testPasses; pass++)
        {
            ct.ThrowIfCancellationRequested();
            
            var speed = RunRandom4KWriteTest(path, ct, pass);
            results.Add(speed);
            
            if (pass < _testPasses - 1)
                Thread.Sleep(100);
        }
        
        return results.Average();
    }

    private double RunRandom4KWriteTest(string path, CancellationToken ct, int passNumber)
    {
        var buffer = new byte[Random4KBlockSize];
        new Random().NextBytes(buffer);

        // Use unbuffered I/O for random writes
        using var handle = CreateFile(
            path,
            GENERIC_WRITE,
            0,
            IntPtr.Zero,
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH,
            IntPtr.Zero);

        if (handle.IsInvalid)
        {
            throw new IOException($"Failed to open file for writing. Error: {Marshal.GetLastWin32Error()}");
        }

        using var fs = new FileStream(handle, FileAccess.Write, Random4KBlockSize, false);

        var fileSize = new FileInfo(path).Length;
        var random = new Random();
        var sw = Stopwatch.StartNew();
        long totalBytesWritten = 0;

        for (int i = 0; i < _random4KIterations; i++)
        {
            ct.ThrowIfCancellationRequested();

            // Random position aligned to 4K (required for unbuffered I/O)
            long maxBlocks = fileSize / Random4KBlockSize - 1;
            long position = (long)(random.NextDouble() * maxBlocks) * Random4KBlockSize;

            fs.Seek(position, SeekOrigin.Begin);
            fs.Write(buffer, 0, buffer.Length);
            totalBytesWritten += buffer.Length;

            if (i % 128 == 0)
            {
                var elapsed = sw.Elapsed.TotalSeconds;
                var currentSpeed = elapsed > 0 ? totalBytesWritten / elapsed / (1024 * 1024) : 0;
                var passProgress = (double)i / _random4KIterations;
                var overallProgress = 50 + ((double)passNumber + passProgress) / _testPasses * 25;
                ReportProgress($"Random 4K Write (Pass {passNumber + 1}/{_testPasses})", overallProgress, currentSpeed);
            }
        }

        fs.Flush();
        sw.Stop();

        var totalSeconds = sw.Elapsed.TotalSeconds;
        return totalSeconds > 0 ? totalBytesWritten / totalSeconds / (1024 * 1024) : 0;
    }

    private double RunRandom4KReadTestMultiPass(string path, CancellationToken ct)
    {
        var results = new List<double>();
        
        for (int pass = 0; pass < _testPasses; pass++)
        {
            ct.ThrowIfCancellationRequested();
            
            var speed = RunRandom4KReadTest(path, ct, pass);
            results.Add(speed);
            
            if (pass < _testPasses - 1)
                Thread.Sleep(100);
        }
        
        return results.Average();
    }

    private double RunRandom4KReadTest(string path, CancellationToken ct, int passNumber)
    {
        var buffer = new byte[Random4KBlockSize];

        // Use unbuffered I/O for random reads
        using var handle = CreateFile(
            path,
            GENERIC_READ,
            FILE_SHARE_READ,
            IntPtr.Zero,
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING,
            IntPtr.Zero);

        if (handle.IsInvalid)
        {
            throw new IOException($"Failed to open file for reading. Error: {Marshal.GetLastWin32Error()}");
        }

        using var fs = new FileStream(handle, FileAccess.Read, Random4KBlockSize, false);

        var fileSize = new FileInfo(path).Length;
        var random = new Random();
        var sw = Stopwatch.StartNew();
        long totalBytesRead = 0;

        for (int i = 0; i < _random4KIterations; i++)
        {
            ct.ThrowIfCancellationRequested();

            // Random position aligned to 4K (required for unbuffered I/O)
            long maxBlocks = fileSize / Random4KBlockSize - 1;
            long position = (long)(random.NextDouble() * maxBlocks) * Random4KBlockSize;

            fs.Seek(position, SeekOrigin.Begin);
            int bytesRead = fs.Read(buffer, 0, buffer.Length);
            totalBytesRead += bytesRead;

            if (i % 128 == 0)
            {
                var elapsed = sw.Elapsed.TotalSeconds;
                var currentSpeed = elapsed > 0 ? totalBytesRead / elapsed / (1024 * 1024) : 0;
                var passProgress = (double)i / _random4KIterations;
                var overallProgress = 75 + ((double)passNumber + passProgress) / _testPasses * 25;
                ReportProgress($"Random 4K Read (Pass {passNumber + 1}/{_testPasses})", overallProgress, currentSpeed);
            }
        }

        sw.Stop();

        var totalSeconds = sw.Elapsed.TotalSeconds;
        return totalSeconds > 0 ? totalBytesRead / totalSeconds / (1024 * 1024) : 0;
    }

    private void ReportProgress(string operation, double progressPercent, double currentSpeedMBps)
    {
        ProgressUpdated?.Invoke(this, new SpeedTestProgress
        {
            CurrentOperation = operation,
            ProgressPercent = progressPercent,
            CurrentSpeedMBps = currentSpeedMBps
        });
    }
}
