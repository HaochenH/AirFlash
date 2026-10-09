using AirFlash.Core;
using System.Reflection;
using System.Diagnostics;

namespace AirFlash.UI.Services;

/// <summary>
/// Linux paths and the native engine location. Everything lives under the XDG
/// base directories; nothing is written next to the executable.
/// </summary>
public static class AppPaths
{
    public const string Name = "AirFlash";
    public static string Version => typeof(AppPaths).Assembly.GetName().Version!.ToString(3);
    public static string VersionLabel => $"AirFlash {Version}{(IsPreview ? " · Preview" : "")} · Avalonia/Linux";
    public static bool IsPreview => typeof(AppPaths).Assembly
        .GetCustomAttribute<System.Reflection.AssemblyInformationalVersionAttribute>()?
        .InformationalVersion.Contains("-rc.", StringComparison.OrdinalIgnoreCase) == true;

    private static string Home => Environment.GetEnvironmentVariable("HOME") ?? Environment.CurrentDirectory;

    public static string DataDirectory { get; } = Path.Combine(
        Environment.GetEnvironmentVariable("AIRFLASH_DATA_DIR") is { Length: > 0 } custom ? custom
            : Environment.GetEnvironmentVariable("XDG_CONFIG_HOME") is { Length: > 0 } config ? config
            : Path.Combine(Home, ".config"),
        Name);
    public static string SettingsPath => Path.Combine(DataDirectory, "settings.json");
    public static string LogDirectory => Path.Combine(StateDirectory, "logs");
    public static string StateDirectory { get; } = Path.Combine(
        Environment.GetEnvironmentVariable("AIRFLASH_RUNTIME_DIR") is { Length: > 0 } runtime ? runtime
            : Environment.GetEnvironmentVariable("XDG_RUNTIME_DIR") is { Length: > 0 } xdg ? xdg
            : Path.Combine(Home, ".local/state"),
        "airflash");

    public static void Log(string message)
    {
        try
        {
            lock (LogLock)
            {
                Directory.CreateDirectory(LogDirectory);
                File.AppendAllText(
                    Path.Combine(LogDirectory, $"ui-{DateTime.Today:yyyy-MM-dd}.log"),
                    $"{DateTime.Now:O} {message}{Environment.NewLine}");
            }
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { }
    }
    private static readonly object LogLock = new();

    /// <summary>
    /// Resolves the JSONL engine binary: explicit override, then next to this
    /// executable (AppImage / usr/bin), then PATH. On Linux the binary has no
    /// .exe suffix, unlike the Windows build.
    /// </summary>
    /// <summary>
    /// Directories that hold the native binaries inside an install layout: the
    /// executable's own directory, then usr/bin relative to a lib subdirectory
    /// (the AppImage GUI lives in usr/lib/airflash-ui).
    /// </summary>
    private static IEnumerable<string> ExecutableDirectories(string binary)
    {
        var directory = Path.GetDirectoryName(Environment.ProcessPath);
        if (string.IsNullOrEmpty(directory)) yield break;
        yield return Path.Combine(directory, binary);
        yield return Path.Combine(directory, "..", "bin", binary);
        yield return Path.Combine(directory, "..", "..", "bin", binary);
    }

    public static string EnginePath()
    {
        var candidates = new List<string>();
        if (Environment.GetEnvironmentVariable("AIRFLASH_ENGINE") is { Length: > 0 } explicitPath)
            candidates.Add(explicitPath);
        candidates.AddRange(ExecutableDirectories("airflash-engine"));
        foreach (var directoryInPath in (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator))
        {
            if (!string.IsNullOrWhiteSpace(directoryInPath)) candidates.Add(Path.Combine(directoryInPath, "airflash-engine"));
        }
        var found = candidates.FirstOrDefault(File.Exists);
        if (found is null)
            throw new FileNotFoundException(L.Get("The native audio engine was not found. Install AirFlash or set AIRFLASH_ENGINE."));
        return Path.GetFullPath(found);
    }

    /// <summary>The headless CLI shares the same mDNS parser; discovery reuses it.</summary>
    public static string CliPath()
    {
        var candidates = new List<string>();
        if (Environment.GetEnvironmentVariable("AIRFLASH_CLI") is { Length: > 0 } explicitPath)
            candidates.Add(explicitPath);
        candidates.AddRange(ExecutableDirectories("airflash-cli"));
        foreach (var entry in (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator))
        {
            if (!string.IsNullOrWhiteSpace(entry)) candidates.Add(Path.Combine(entry, "airflash-cli"));
        }
        return candidates.FirstOrDefault(File.Exists) ?? "airflash-cli";
    }

    public static void OpenLogs()
    {
        Directory.CreateDirectory(LogDirectory);
        Process.Start(new ProcessStartInfo(LogDirectory) { UseShellExecute = true })?.Dispose();
    }
}
