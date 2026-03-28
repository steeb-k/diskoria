using System.Collections.ObjectModel;
using System.IO;
using System.Management;
using JFStorageTester.Models;

namespace JFStorageTester.Services;

public class DriveDetectionService : IDisposable
{
    private static DriveDetectionService? _instance;
    private static readonly object _lock = new();
    
    public static DriveDetectionService Instance
    {
        get
        {
            if (_instance == null)
            {
                lock (_lock)
                {
                    _instance ??= new DriveDetectionService();
                }
            }
            return _instance;
        }
    }
    
    public ObservableCollection<PhysicalDiskModel> Drives { get; } = new();
    public event EventHandler? DrivesChanged;
    
    private ManagementEventWatcher? _insertWatcher;
    private ManagementEventWatcher? _removeWatcher;
    private bool _disposed;
    
    private DriveDetectionService()
    {
        // Load drives
        var disks = GetPhysicalDisks();
        foreach (var disk in disks)
        {
            Drives.Add(disk);
        }
        
        // Start watching for drive changes
        StartWatching();
    }
    
    public void RefreshDrives()
    {
        Task.Run(() =>
        {
            try
            {
                var currentDisks = GetPhysicalDisks();
                
                var dispatcher = System.Windows.Application.Current?.Dispatcher;
                if (dispatcher == null) return;
                
                dispatcher.BeginInvoke(() =>
                {
                    Drives.Clear();
                    foreach (var disk in currentDisks)
                    {
                        Drives.Add(disk);
                    }
                    DrivesChanged?.Invoke(this, EventArgs.Empty);
                });
            }
            catch
            {
                // Silently handle refresh errors
            }
        });
    }
    
    private List<PhysicalDiskModel> GetPhysicalDisks()
    {
        var disks = new List<PhysicalDiskModel>();
        
        try
        {
            // Step 1: Get partition styles from MSFT_Disk
            var partitionStyles = GetDiskPartitionStyles();
            
            // Step 2: Get media types from MSFT_PhysicalDisk  
            var mediaTypes = GetDiskMediaTypes();
            
            // Step 3: Get BitLocker status for all volumes
            var bitLockerStatuses = GetAllBitLockerStatuses();
            
            // Step 4: Enumerate physical disks via WMI Win32_DiskDrive
            using var diskSearcher = new ManagementObjectSearcher("SELECT * FROM Win32_DiskDrive");
            
            foreach (ManagementObject disk in diskSearcher.Get())
            {
                try
                {
                    var deviceId = disk["DeviceID"]?.ToString() ?? "";
                    var index = Convert.ToInt32(disk["Index"]);
                    var model = disk["Model"]?.ToString()?.Trim() ?? "Unknown Disk";
                    var serial = disk["SerialNumber"]?.ToString()?.Trim() ?? "";
                    var totalSize = Convert.ToInt64(disk["Size"] ?? 0);
                    var mediaType = disk["MediaType"]?.ToString() ?? "";
                    var interfaceType = disk["InterfaceType"]?.ToString() ?? "";
                    
                    var diskModel = new PhysicalDiskModel
                    {
                        DiskNumber = index,
                        DeviceId = deviceId,
                        Model = model,
                        SerialNumber = serial,
                        TotalSize = totalSize,
                        DriveType = DetermineStorageType(index, mediaType, interfaceType, model, mediaTypes),
                        PartitionScheme = partitionStyles.TryGetValue(index, out var ps) ? ps : PartitionScheme.Unknown,
                        Partitions = new List<PartitionInfoModel>()
                    };
                    
                    // Step 5: Get partitions for this disk
                    try
                    {
                        using var partSearcher = new ManagementObjectSearcher(
                            $"ASSOCIATORS OF {{Win32_DiskDrive.DeviceID='{deviceId}'}} WHERE AssocClass=Win32_DiskDriveToDiskPartition");
                        
                        foreach (ManagementObject partition in partSearcher.Get())
                        {
                            var partId = partition["DeviceID"]?.ToString() ?? "";
                            
                            using var logicalSearcher = new ManagementObjectSearcher(
                                $"ASSOCIATORS OF {{Win32_DiskPartition.DeviceID='{partId}'}} WHERE AssocClass=Win32_LogicalDiskToPartition");
                            
                            foreach (ManagementObject logical in logicalSearcher.Get())
                            {
                                var driveLetter = logical["DeviceID"]?.ToString() ?? "";
                                if (string.IsNullOrEmpty(driveLetter)) continue;
                                
                                try
                                {
                                    var driveInfo = new DriveInfo(driveLetter);
                                    if (!driveInfo.IsReady) continue;
                                    
                                    var partInfo = new PartitionInfoModel
                                    {
                                        DriveLetter = driveLetter,
                                        VolumeName = string.IsNullOrEmpty(driveInfo.VolumeLabel)
                                            ? "Local Disk"
                                            : driveInfo.VolumeLabel,
                                        TotalSize = driveInfo.TotalSize,
                                        FreeSpace = driveInfo.TotalFreeSpace,
                                        UsedSpace = driveInfo.TotalSize - driveInfo.TotalFreeSpace,
                                        FileSystem = driveInfo.DriveFormat,
                                        IsSystemPartition = IsSystemDrive(driveLetter),
                                        BitLockerStatus = bitLockerStatuses.TryGetValue(driveLetter, out var bls) 
                                            ? bls 
                                            : BitLockerStatus.NotEncrypted
                                    };
                                    
                                    diskModel.Partitions.Add(partInfo);
                                }
                                catch
                                {
                                    // Skip inaccessible partitions
                                }
                            }
                        }
                    }
                    catch
                    {
                        // Skip partition enumeration errors
                    }
                    
                    // Sort partitions by drive letter
                    diskModel.Partitions = diskModel.Partitions.OrderBy(p => p.DriveLetter).ToList();
                    
                    disks.Add(diskModel);
                }
                catch
                {
                    // Skip problematic disks
                }
            }
        }
        catch
        {
            // Return empty list if enumeration fails entirely
        }
        
        return disks.OrderBy(d => d.DiskNumber).ToList();
    }
    
    private Dictionary<int, PartitionScheme> GetDiskPartitionStyles()
    {
        var styles = new Dictionary<int, PartitionScheme>();
        try
        {
            using var searcher = new ManagementObjectSearcher(
                @"root\Microsoft\Windows\Storage",
                "SELECT Number, PartitionStyle FROM MSFT_Disk");
            
            foreach (ManagementObject disk in searcher.Get())
            {
                var number = Convert.ToInt32(disk["Number"]);
                var style = Convert.ToInt32(disk["PartitionStyle"]);
                styles[number] = style switch
                {
                    1 => PartitionScheme.MBR,
                    2 => PartitionScheme.GPT,
                    _ => PartitionScheme.Unknown
                };
            }
        }
        catch { }
        return styles;
    }
    
    private Dictionary<int, int> GetDiskMediaTypes()
    {
        var types = new Dictionary<int, int>();
        try
        {
            using var searcher = new ManagementObjectSearcher(
                @"root\Microsoft\Windows\Storage",
                "SELECT DeviceId, MediaType FROM MSFT_PhysicalDisk");
            
            foreach (ManagementObject disk in searcher.Get())
            {
                var deviceId = Convert.ToInt32(disk["DeviceId"]);
                var mediaType = Convert.ToInt32(disk["MediaType"]);
                types[deviceId] = mediaType;
            }
        }
        catch { }
        return types;
    }
    
    private Dictionary<string, BitLockerStatus> GetAllBitLockerStatuses()
    {
        var statuses = new Dictionary<string, BitLockerStatus>();
        try
        {
            using var searcher = new ManagementObjectSearcher(
                @"\\.\root\cimv2\Security\MicrosoftVolumeEncryption",
                "SELECT DriveLetter, ProtectionStatus, ConversionStatus FROM Win32_EncryptableVolume");
            
            foreach (ManagementObject volume in searcher.Get())
            {
                var letter = volume["DriveLetter"]?.ToString() ?? "";
                if (string.IsNullOrEmpty(letter)) continue;
                
                var protectionStatus = Convert.ToInt32(volume["ProtectionStatus"]);
                var conversionStatus = Convert.ToInt32(volume["ConversionStatus"]);
                
                if (conversionStatus == 0)
                    statuses[letter] = BitLockerStatus.NotEncrypted;
                else if (protectionStatus == 1)
                    statuses[letter] = BitLockerStatus.Locked;
                else if (conversionStatus == 1 && protectionStatus == 0)
                    statuses[letter] = BitLockerStatus.Unlocked;
                else
                    statuses[letter] = BitLockerStatus.Encrypted;
            }
        }
        catch { }
        return statuses;
    }
    
    private StorageType DetermineStorageType(int diskIndex, string wmiMediaType, string interfaceType, string model, Dictionary<int, int> msftMediaTypes)
    {
        var modelLower = model.ToLower();
        var interfaceLower = interfaceType.ToLower();
        var mediaLower = wmiMediaType.ToLower();
        
        // Check MSFT_PhysicalDisk MediaType first (most reliable)
        if (msftMediaTypes.TryGetValue(diskIndex, out var msftType))
        {
            // MediaType: 0 = Unspecified, 3 = HDD, 4 = SSD, 5 = SCM
            if (msftType == 4) return StorageType.SSD;
            if (msftType == 3) return StorageType.HDD;
        }
        
        // Check for NVMe
        if (interfaceLower.Contains("nvme") || modelLower.Contains("nvme"))
            return StorageType.NVMe;
        
        // Check for eMMC
        if (modelLower.Contains("emmc") || mediaLower.Contains("emmc"))
            return StorageType.eMMC;
        
        // Check for SSD indicators
        if (mediaLower.Contains("ssd") || mediaLower.Contains("solid") ||
            modelLower.Contains("ssd"))
            return StorageType.SSD;
        
        // Check if removable
        if (mediaLower.Contains("removable") || mediaLower.Contains("external"))
            return StorageType.USB;
        
        // Check for HDD
        if (mediaLower.Contains("hard") || mediaLower.Contains("fixed") || mediaLower.Contains("hdd"))
            return StorageType.HDD;
        
        // Default: if fixed hard disk media, assume HDD
        if (mediaLower.Contains("fixed"))
            return StorageType.HDD;
        
        return StorageType.Unknown;
    }
    
    private bool IsSystemDrive(string driveLetter)
    {
        var systemDrive = Environment.GetFolderPath(Environment.SpecialFolder.Windows);
        var letter = driveLetter.TrimEnd(':', '\\') + ":\\";
        return systemDrive.StartsWith(letter, StringComparison.OrdinalIgnoreCase)
            || systemDrive.StartsWith(driveLetter, StringComparison.OrdinalIgnoreCase);
    }
    
    private void StartWatching()
    {
        try
        {
            var insertQuery = new WqlEventQuery(
                "SELECT * FROM __InstanceCreationEvent WITHIN 2 WHERE TargetInstance ISA 'Win32_DiskDrive'");
            _insertWatcher = new ManagementEventWatcher(insertQuery);
            _insertWatcher.EventArrived += (s, e) => RefreshDrives();
            _insertWatcher.Start();
            
            var removeQuery = new WqlEventQuery(
                "SELECT * FROM __InstanceDeletionEvent WITHIN 2 WHERE TargetInstance ISA 'Win32_DiskDrive'");
            _removeWatcher = new ManagementEventWatcher(removeQuery);
            _removeWatcher.EventArrived += (s, e) => RefreshDrives();
            _removeWatcher.Start();
        }
        catch { }
    }
    
    public void Dispose()
    {
        if (_disposed) return;
        
        _insertWatcher?.Stop();
        _insertWatcher?.Dispose();
        _removeWatcher?.Stop();
        _removeWatcher?.Dispose();
        
        _disposed = true;
    }
}
