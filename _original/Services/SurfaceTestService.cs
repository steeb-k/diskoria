using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace JFStorageTester.Services;

public class SurfaceTestProgress
{
    public double ProgressPercent { get; set; }
    public long BytesScanned { get; set; }
    public long TotalBytes { get; set; }
    public long GoodSectors { get; set; }
    public long BadSectors { get; set; }
    public long SlowSectors { get; set; }
    public double AverageSpeedMBps { get; set; }
    public double CurrentSpeedMBps { get; set; }
    public int BlockIndex { get; set; }
    public bool BlockIsGood { get; set; }
    public double BlockReadTimeMs { get; set; }
    public long TotalSectors { get; set; }
}

public class SurfaceTestService
{
    // P/Invoke for direct disk access (read-only)
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Auto)]
    private static extern SafeFileHandle CreateFile(
        string lpFileName,
        uint dwDesiredAccess,
        uint dwShareMode,
        IntPtr lpSecurityAttributes,
        uint dwCreationDisposition,
        uint dwFlagsAndAttributes,
        IntPtr hTemplateFile);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool ReadFile(
        SafeFileHandle hFile,
        byte[] lpBuffer,
        uint nNumberOfBytesToRead,
        out uint lpNumberOfBytesRead,
        IntPtr lpOverlapped);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetFilePointerEx(
        SafeFileHandle hFile,
        long liDistanceToMove,
        out long lpNewFilePointer,
        uint dwMoveMethod);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool DeviceIoControl(
        SafeFileHandle hDevice,
        uint dwIoControlCode,
        IntPtr lpInBuffer,
        uint nInBufferSize,
        IntPtr lpOutBuffer,
        uint nOutBufferSize,
        out uint lpBytesReturned,
        IntPtr lpOverlapped);

    private const uint GENERIC_READ = 0x80000000;
    private const uint FILE_SHARE_READ = 0x00000001;
    private const uint FILE_SHARE_WRITE = 0x00000002;
    private const uint OPEN_EXISTING = 3;
    private const uint FILE_FLAG_NO_BUFFERING = 0x20000000;
    private const uint FILE_FLAG_SEQUENTIAL_SCAN = 0x08000000;
    private const uint IOCTL_DISK_GET_DRIVE_GEOMETRY_EX = 0x000700A0;

    [StructLayout(LayoutKind.Sequential)]
    private struct DISK_GEOMETRY
    {
        public long Cylinders;
        public uint MediaType;
        public uint TracksPerCylinder;
        public uint SectorsPerTrack;
        public uint BytesPerSector;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct DISK_GEOMETRY_EX
    {
        public DISK_GEOMETRY Geometry;
        public long DiskSize;
    }

    private CancellationTokenSource? _cts;
    private bool _isRunning;

    public bool IsRunning => _isRunning;

    public event EventHandler<SurfaceTestProgress>? ProgressUpdated;
    public event EventHandler? TestCompleted;
    public event EventHandler<string>? ErrorOccurred;

    /// <summary>
    /// Starts a READ-ONLY surface test on the specified physical drive.
    /// Accepts a physical drive path like \\.\PhysicalDrive0
    /// This performs a FULL sequential read of the entire drive surface.
    /// This will NOT write any data to the drive - it only reads sectors.
    /// </summary>
    public async Task StartTestAsync(string physicalDrivePath, int totalBlocks = 1000)
    {
        if (_isRunning)
            return;

        _isRunning = true;
        _cts = new CancellationTokenSource();

        try
        {
            await Task.Run(() => RunPhysicalDriveSurfaceTest(physicalDrivePath, totalBlocks, _cts.Token));
        }
        catch (OperationCanceledException)
        {
            // Test was cancelled, this is expected
        }
        catch (Exception ex)
        {
            ErrorOccurred?.Invoke(this, ex.Message);
        }
        finally
        {
            _isRunning = false;
            TestCompleted?.Invoke(this, EventArgs.Empty);
        }
    }

    public void StopTest()
    {
        _cts?.Cancel();
    }

    private void RunPhysicalDriveSurfaceTest(string physicalDrive, int totalBlocks, CancellationToken ct)
    {
        using var handle = CreateFile(
            physicalDrive,
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            IntPtr.Zero,
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN,
            IntPtr.Zero);

        if (handle.IsInvalid)
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(),
                $"Cannot open physical drive {physicalDrive}. Try running as Administrator.");
        }

        var geometryEx = new DISK_GEOMETRY_EX();
        var geometrySize = (uint)Marshal.SizeOf(geometryEx);
        var geometryPtr = Marshal.AllocHGlobal((int)geometrySize);

        try
        {
            if (DeviceIoControl(handle, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IntPtr.Zero, 0,
                geometryPtr, geometrySize, out _, IntPtr.Zero))
            {
                geometryEx = Marshal.PtrToStructure<DISK_GEOMETRY_EX>(geometryPtr);
                var totalSize = geometryEx.DiskSize;
                var bytesPerSector = (int)geometryEx.Geometry.BytesPerSector;

                PerformFullSequentialRead(handle, totalSize, bytesPerSector, totalBlocks, ct);
            }
            else
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(),
                    $"Cannot read disk geometry for {physicalDrive}. Try running as Administrator.");
            }
        }
        finally
        {
            Marshal.FreeHGlobal(geometryPtr);
        }
    }

    /// <summary>
    /// Performs a FULL sequential read of the entire drive.
    /// This reads every sector from start to finish, just like HD Sentinel.
    /// </summary>
    private void PerformFullSequentialRead(SafeFileHandle handle, long totalSize, int bytesPerSector, int totalBlocks, CancellationToken ct)
    {
        // Use 64KB chunks for reading - small enough for granularity, large enough for efficiency
        const int chunkSize = 64 * 1024; // 64 KB
        var alignedChunkSize = (chunkSize / bytesPerSector) * bytesPerSector;
        if (alignedChunkSize == 0) alignedChunkSize = bytesPerSector;

        var totalSectors = totalSize / bytesPerSector;
        
        // Slow sector threshold in milliseconds (per 64KB chunk)
        const double slowThresholdMs = 200.0;

        var buffer = new byte[alignedChunkSize];
        var stopwatch = new Stopwatch();

        long goodSectors = 0;
        long badSectors = 0;
        long slowSectors = 0;
        long totalBytesRead = 0;
        long currentPosition = 0;
        var overallStopwatch = Stopwatch.StartNew();

        // Track per-block state using position-based block index calculation
        int lastBlockIndex = -1;
        bool currentBlockHasBadSectors = false;
        double currentBlockTotalTimeMs = 0;
        int currentBlockReadCount = 0;

        // Seek to beginning
        SetFilePointerEx(handle, 0, out _, 0);

        while (currentPosition < totalSize)
        {
            ct.ThrowIfCancellationRequested();

            // Calculate how much to read (may be less at end of drive)
            var bytesToRead = (int)Math.Min(alignedChunkSize, totalSize - currentPosition);
            
            // Align to sector boundary
            bytesToRead = (bytesToRead / bytesPerSector) * bytesPerSector;
            if (bytesToRead == 0) break;

            // Read and measure time
            stopwatch.Restart();
            var readSuccess = ReadFile(handle, buffer, (uint)bytesToRead, out var bytesRead, IntPtr.Zero);
            stopwatch.Stop();

            var readTimeMs = stopwatch.Elapsed.TotalMilliseconds;
            if (readTimeMs < 0.01) readTimeMs = 0.01;

            var sectorsInThisRead = bytesRead / (uint)bytesPerSector;

            if (readSuccess && bytesRead > 0)
            {
                // Check if this chunk was slow
                if (readTimeMs >= slowThresholdMs)
                {
                    slowSectors += sectorsInThisRead;
                }
                else
                {
                    goodSectors += sectorsInThisRead;
                }
                totalBytesRead += bytesRead;
                currentPosition += bytesRead;
            }
            else
            {
                // Read failed - try to identify which sectors are bad by reading smaller chunks
                var badSectorsFound = HandleReadError(handle, currentPosition, bytesToRead, bytesPerSector, ct);
                badSectors += badSectorsFound;
                goodSectors += (bytesToRead / bytesPerSector) - badSectorsFound;
                currentPosition += bytesToRead;
                currentBlockHasBadSectors = true;

                // Re-seek to continue after the bad area
                SetFilePointerEx(handle, currentPosition, out _, 0);
            }

            currentBlockTotalTimeMs += readTimeMs;
            currentBlockReadCount++;

            // Calculate which block this position maps to
            // Use: blockIndex = (int)((double)currentPosition / totalSize * totalBlocks)
            // Clamped to [0, totalBlocks-1]
            int blockIndex = (int)((double)currentPosition / totalSize * totalBlocks);
            if (blockIndex >= totalBlocks) blockIndex = totalBlocks - 1;
            if (blockIndex < 0) blockIndex = 0;

            // Did we cross into a new block (or is this the final position)?
            if (blockIndex != lastBlockIndex || currentPosition >= totalSize)
            {
                // Emit a progress update for the completed block
                int reportBlockIndex = lastBlockIndex >= 0 ? lastBlockIndex : blockIndex;
                if (lastBlockIndex < 0) reportBlockIndex = 0;

                var avgBlockReadTime = currentBlockReadCount > 0 ? currentBlockTotalTimeMs / currentBlockReadCount : 0;
                var progressPercent = (currentPosition * 100.0) / totalSize;
                var elapsedSeconds = overallStopwatch.Elapsed.TotalSeconds;
                var avgSpeedMBps = elapsedSeconds > 0 ? (totalBytesRead / (1024.0 * 1024.0)) / elapsedSeconds : 0;
                var currentSpeedMBps = readTimeMs > 0 ? (bytesToRead / (1024.0 * 1024.0)) / (readTimeMs / 1000.0) : 0;

                var progress = new SurfaceTestProgress
                {
                    ProgressPercent = progressPercent,
                    BytesScanned = currentPosition,
                    TotalBytes = totalSize,
                    TotalSectors = totalSectors,
                    GoodSectors = goodSectors,
                    BadSectors = badSectors,
                    SlowSectors = slowSectors,
                    AverageSpeedMBps = avgSpeedMBps,
                    CurrentSpeedMBps = currentSpeedMBps,
                    BlockIndex = reportBlockIndex,
                    BlockIsGood = !currentBlockHasBadSectors,
                    BlockReadTimeMs = avgBlockReadTime
                };

                ProgressUpdated?.Invoke(this, progress);

                // Reset per-block accumulators for the new block
                lastBlockIndex = blockIndex;
                currentBlockHasBadSectors = false;
                currentBlockTotalTimeMs = 0;
                currentBlockReadCount = 0;
            }
        }
    }

    /// <summary>
    /// When a read fails, try smaller reads to identify exactly which sectors are bad.
    /// </summary>
    private long HandleReadError(SafeFileHandle handle, long position, int bytesToRead, int bytesPerSector, CancellationToken ct)
    {
        long badSectorCount = 0;
        var singleSectorBuffer = new byte[bytesPerSector];
        var endPosition = position + bytesToRead;

        // Try reading sector by sector to identify bad sectors
        for (long sectorPos = position; sectorPos < endPosition; sectorPos += bytesPerSector)
        {
            ct.ThrowIfCancellationRequested();

            if (!SetFilePointerEx(handle, sectorPos, out _, 0))
            {
                badSectorCount++;
                continue;
            }

            if (!ReadFile(handle, singleSectorBuffer, (uint)bytesPerSector, out var bytesRead, IntPtr.Zero) || bytesRead == 0)
            {
                badSectorCount++;
            }
        }

        return badSectorCount;
    }

}
