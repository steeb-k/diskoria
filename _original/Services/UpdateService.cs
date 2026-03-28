using System.Diagnostics;
using System.IO;
using System.Net.Http;
using System.Net.Http.Json;
using System.Reflection;
using System.Text.Json.Serialization;

namespace JFStorageTester.Services;

public class GitHubRelease
{
    [JsonPropertyName("tag_name")]
    public string TagName { get; set; } = "";
    
    [JsonPropertyName("name")]
    public string Name { get; set; } = "";
    
    [JsonPropertyName("body")]
    public string Body { get; set; } = "";
    
    [JsonPropertyName("html_url")]
    public string HtmlUrl { get; set; } = "";
    
    [JsonPropertyName("assets")]
    public List<GitHubAsset> Assets { get; set; } = new();
}

public class GitHubAsset
{
    [JsonPropertyName("name")]
    public string Name { get; set; } = "";
    
    [JsonPropertyName("browser_download_url")]
    public string DownloadUrl { get; set; } = "";
    
    [JsonPropertyName("size")]
    public long Size { get; set; }
}

public class UpdateCheckResult
{
    public bool UpdateAvailable { get; set; }
    public string CurrentVersion { get; set; } = "";
    public string LatestVersion { get; set; } = "";
    public string ReleaseNotes { get; set; } = "";
    public string DownloadUrl { get; set; } = "";
    public string ReleaseUrl { get; set; } = "";
    public long DownloadSize { get; set; }
    public string? ErrorMessage { get; set; }
}

public class UpdateService
{
    private const string GitHubOwner = "jjfbno";
    private const string GitHubRepo = "JF-Storage-Tester";
    private const string GitHubApiUrl = $"https://api.github.com/repos/{GitHubOwner}/{GitHubRepo}/releases/latest";
    
    private readonly HttpClient _httpClient;
    
    public event EventHandler<double>? DownloadProgressChanged;
    
    public UpdateService()
    {
        _httpClient = new HttpClient();
        _httpClient.DefaultRequestHeaders.Add("User-Agent", "JFStorageTester");
    }
    
    public static string CurrentVersion
    {
        get
        {
            var version = Assembly.GetExecutingAssembly().GetName().Version;
            if (version == null) return "1.0.0";
            return version.Revision > 0
                ? $"{version.Major}.{version.Minor}.{version.Build}.{version.Revision}"
                : $"{version.Major}.{version.Minor}.{version.Build}";
        }
    }
    
    public async Task<UpdateCheckResult> CheckForUpdatesAsync()
    {
        var result = new UpdateCheckResult
        {
            CurrentVersion = CurrentVersion
        };
        
        try
        {
            var response = await _httpClient.GetAsync(GitHubApiUrl);
            
            if (!response.IsSuccessStatusCode)
            {
                if (response.StatusCode == System.Net.HttpStatusCode.NotFound)
                {
                    // No releases yet
                    result.LatestVersion = CurrentVersion;
                    result.UpdateAvailable = false;
                    return result;
                }
                result.ErrorMessage = $"GitHub API returned {response.StatusCode}";
                return result;
            }
            
            var release = await response.Content.ReadFromJsonAsync<GitHubRelease>();
            
            if (release == null)
            {
                result.ErrorMessage = "Could not parse release information";
                return result;
            }
            
            // Parse version from tag (remove 'v' prefix if present)
            var latestVersion = release.TagName.TrimStart('v', 'V');
            result.LatestVersion = latestVersion;
            result.ReleaseNotes = release.Body;
            result.ReleaseUrl = release.HtmlUrl;
            
            // Find the Setup installer asset (preferred for updates)
            var setupAsset = release.Assets.FirstOrDefault(a =>
                a.Name.Contains("Setup", StringComparison.OrdinalIgnoreCase) &&
                a.Name.EndsWith(".exe", StringComparison.OrdinalIgnoreCase));
            
            // Fall back to portable exe if no installer
            setupAsset ??= release.Assets.FirstOrDefault(a =>
                a.Name.EndsWith(".exe", StringComparison.OrdinalIgnoreCase));
            
            if (setupAsset != null)
            {
                result.DownloadUrl = setupAsset.DownloadUrl;
                result.DownloadSize = setupAsset.Size;
            }
            else
            {
                result.ErrorMessage = "Update available but no download file attached to release. Please download manually from GitHub.";
                return result;
            }
            
            // Compare versions
            result.UpdateAvailable = IsNewerVersion(latestVersion, CurrentVersion);
        }
        catch (HttpRequestException ex)
        {
            result.ErrorMessage = $"Network error: {ex.Message}";
        }
        catch (Exception ex)
        {
            result.ErrorMessage = $"Error checking for updates: {ex.Message}";
        }
        
        return result;
    }
    
    public async Task<bool> DownloadAndInstallUpdateAsync(string downloadUrl, CancellationToken ct = default)
    {
        try
        {
            // Download to temp folder so there's no file locking issue
            var tempDir = Path.Combine(Path.GetTempPath(), "JFStorageTester_Update");
            Directory.CreateDirectory(tempDir);
            
            var fileName = Path.GetFileName(new Uri(downloadUrl).LocalPath);
            if (string.IsNullOrEmpty(fileName)) fileName = "JFStorageTester_Setup.exe";
            var installerPath = Path.Combine(tempDir, fileName);
            
            // Download the installer
            using var response = await _httpClient.GetAsync(downloadUrl, HttpCompletionOption.ResponseHeadersRead, ct);
            response.EnsureSuccessStatusCode();
            
            var totalBytes = response.Content.Headers.ContentLength ?? 0;
            var downloadedBytes = 0L;
            
            using (var contentStream = await response.Content.ReadAsStreamAsync(ct))
            using (var fileStream = new FileStream(installerPath, FileMode.Create, FileAccess.Write, FileShare.None, 8192, true))
            {
                var buffer = new byte[8192];
                int bytesRead;
                
                while ((bytesRead = await contentStream.ReadAsync(buffer, 0, buffer.Length, ct)) > 0)
                {
                    await fileStream.WriteAsync(buffer, 0, bytesRead, ct);
                    downloadedBytes += bytesRead;
                    
                    if (totalBytes > 0)
                    {
                        var progress = (double)downloadedBytes / totalBytes * 100;
                        DownloadProgressChanged?.Invoke(this, progress);
                    }
                }
            }
            
            // Release the app mutex BEFORE launching installer
            // so Inno Setup doesn't think we're still running
            App.ReleaseAppMutex();
            
            // Launch the installer with /VERYSILENT (no UI at all)
            var psi = new ProcessStartInfo
            {
                FileName = installerPath,
                Arguments = "/VERYSILENT /CLOSEAPPLICATIONS /RESTARTAPPLICATIONS",
                UseShellExecute = true,
                Verb = "runas" // Request admin elevation
            };
            
            Process.Start(psi);
            
            // Exit immediately so the exe file is unlocked for the installer
            Environment.Exit(0);
            
            return true;
        }
        catch (Exception)
        {
            return false;
        }
    }
    
    private static bool IsNewerVersion(string latest, string current)
    {
        try
        {
            var latestParts = latest.Split('.').Select(int.Parse).ToArray();
            var currentParts = current.Split('.').Select(int.Parse).ToArray();
            
            // Pad arrays to same length
            var maxLen = Math.Max(latestParts.Length, currentParts.Length);
            Array.Resize(ref latestParts, maxLen);
            Array.Resize(ref currentParts, maxLen);
            
            for (int i = 0; i < maxLen; i++)
            {
                if (latestParts[i] > currentParts[i]) return true;
                if (latestParts[i] < currentParts[i]) return false;
            }
            
            return false; // Versions are equal
        }
        catch
        {
            return false; // If parsing fails, assume no update
        }
    }
}
