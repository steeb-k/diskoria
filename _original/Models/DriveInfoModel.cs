namespace JFStorageTester.Models;

public enum StorageType
{
    Unknown,
    HDD,
    SSD,
    NVMe,
    eMMC,
    USB,
    Optical,
    Network
}

public enum BitLockerStatus
{
    NotEncrypted,
    Encrypted,
    Locked,
    Unlocked
}

public enum PartitionScheme
{
    Unknown,
    MBR,
    GPT
}

/// <summary>
/// Represents a physical disk (e.g., Disk 0, Disk 1) with its partitions.
/// The surface test scans the ENTIRE physical disk.
/// </summary>
public class PhysicalDiskModel
{
    public int DiskNumber { get; set; }
    public string DeviceId { get; set; } = "";  // e.g., \\.\PhysicalDrive0
    public string Model { get; set; } = "";
    public string SerialNumber { get; set; } = "";
    public long TotalSize { get; set; }
    public StorageType DriveType { get; set; }
    public PartitionScheme PartitionScheme { get; set; }
    public List<PartitionInfoModel> Partitions { get; set; } = new();
    
    public string DisplayName => $"Disk {DiskNumber} - {Model} ({TotalSizeDisplay})";
    
    public string DriveTypeDisplay => DriveType switch
    {
        StorageType.HDD => "HDD",
        StorageType.SSD => "SSD",
        StorageType.NVMe => "NVMe",
        StorageType.eMMC => "eMMC",
        StorageType.USB => "USB",
        StorageType.Optical => "Optical",
        StorageType.Network => "Network",
        _ => "Unknown"
    };
    
    public string PartitionSchemeDisplay => PartitionScheme switch
    {
        PartitionScheme.MBR => "MBR",
        PartitionScheme.GPT => "GPT",
        _ => "Unknown"
    };
    
    public string TotalSizeDisplay => FormatBytes(TotalSize);
    
    public long TotalUsedSpace => Partitions.Sum(p => p.UsedSpace);
    public long TotalFreeSpace => Partitions.Sum(p => p.FreeSpace);
    public long TotalPartitionedSpace => Partitions.Sum(p => p.TotalSize);
    
    public string UsedSpaceDisplay => FormatBytes(TotalUsedSpace);
    public string FreeSpaceDisplay => FormatBytes(TotalFreeSpace);
    
    public double UsedPercentage => TotalPartitionedSpace > 0
        ? (double)TotalUsedSpace / TotalPartitionedSpace * 100
        : 0;
    
    public bool IsStorageCritical => UsedPercentage >= 90;
    
    public bool IsSystemDisk => Partitions.Any(p => p.IsSystemPartition);
    
    public bool HasBitLockerLocked => Partitions.Any(p => p.BitLockerStatus == BitLockerStatus.Locked);
    
    /// <summary>
    /// Get the best partition for speed testing (largest non-system partition with most free space).
    /// </summary>
    public PartitionInfoModel? BestSpeedTestPartition =>
        Partitions.Where(p => !p.IsSystemPartition && p.BitLockerStatus != BitLockerStatus.Locked)
                  .OrderByDescending(p => p.FreeSpace)
                  .FirstOrDefault()
        ?? Partitions.Where(p => p.BitLockerStatus != BitLockerStatus.Locked)
                     .OrderByDescending(p => p.FreeSpace)
                     .FirstOrDefault();
    
    /// <summary>
    /// Returns all partitions' drive letters as a comma-separated string.
    /// </summary>
    public string PartitionLetters => string.Join(", ", 
        Partitions.Select(p => p.DriveLetter).Where(l => !string.IsNullOrEmpty(l)));
    
    public string FileSystemDisplay => string.Join(", ",
        Partitions.Select(p => p.FileSystem).Where(f => !string.IsNullOrEmpty(f)).Distinct());
    
    public static string FormatBytes(long bytes)
    {
        string[] sizes = { "B", "KB", "MB", "GB", "TB" };
        int order = 0;
        double size = bytes;
        
        while (size >= 1024 && order < sizes.Length - 1)
        {
            order++;
            size /= 1024;
        }
        
        return $"{size:0.##} {sizes[order]}";
    }
}

/// <summary>
/// Represents a partition/volume on a physical disk.
/// </summary>
public class PartitionInfoModel
{
    public string DriveLetter { get; set; } = "";   // e.g., "C:"
    public string VolumeName { get; set; } = "";
    public long TotalSize { get; set; }
    public long FreeSpace { get; set; }
    public long UsedSpace { get; set; }
    public string FileSystem { get; set; } = "";
    public bool IsSystemPartition { get; set; }
    public BitLockerStatus BitLockerStatus { get; set; }
    
    public string DisplayName => string.IsNullOrEmpty(VolumeName) 
        ? $"{DriveLetter}" 
        : $"{DriveLetter} ({VolumeName})";
    
    public string TotalSizeDisplay => PhysicalDiskModel.FormatBytes(TotalSize);
    public string FreeSpaceDisplay => PhysicalDiskModel.FormatBytes(FreeSpace);
    
    public bool IsBitLockerLocked => BitLockerStatus == BitLockerStatus.Locked;
    
    public string BitLockerDisplay => BitLockerStatus switch
    {
        BitLockerStatus.Locked => "\U0001F512 Locked",
        BitLockerStatus.Unlocked => "\U0001F513 Unlocked",
        BitLockerStatus.Encrypted => "\U0001F510 Encrypted",
        _ => ""
    };
    
    public bool IsBitLockerProtected => BitLockerStatus != BitLockerStatus.NotEncrypted;
}

