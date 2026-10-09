using System.Diagnostics;
using System.Globalization;
using AirFlash.Core;
using System.Text.Json;

namespace AirFlash.UI.Services;

/// <summary>
/// mDNS discovery through the headless CLI's JSON output. The Rust engine already
/// browses _airplay._tcp.local and _raop._tcp.local, so no second mDNS stack is
/// introduced here; this service only turns the JSON into service records and
/// feeds the shared aggregation logic.
/// </summary>
public sealed class LinuxDiscovery : IDiscoveryService
{
    private static readonly TimeSpan Interval = TimeSpan.FromSeconds(5);
    private readonly System.Timers.Timer _timer = new(Interval.TotalMilliseconds) { AutoReset = false };
    private readonly CancellationTokenSource _stop = new();
    private readonly SemaphoreSlim _gate = new(1, 1);
    private Task _loop = Task.CompletedTask;
    private string _interface = "";
    private bool _disposed;

    public event Action<IReadOnlyList<Receiver>>? Changed;
    public event Action<string>? Failed;

    public void SetInterface(string id)
    {
        // mDNS on Linux answers on every interface; a saved interface selection is
        // recorded but does not narrow the query.
        _interface = id;
    }

    public void Start()
    {
        if (_disposed || _loop.IsCompleted == false) return;
        _loop = Task.Run(LoopAsync);
    }

    private async Task LoopAsync()
    {
        while (!_stop.IsCancellationRequested)
        {
            try
            {
                await _gate.WaitAsync(_stop.Token).ConfigureAwait(false);
                try
                {
                    var records = await QueryAsync(_stop.Token).ConfigureAwait(false);
                    if (records is not null) Changed?.Invoke(ReceiverAggregator.Build(records));
                }
                finally { _gate.Release(); }
            }
            catch (OperationCanceledException) { return; }
            catch (Exception error) when (error is IOException or JsonException or InvalidOperationException)
            {
                Failed?.Invoke(error.Message);
            }
            try { await Task.Delay(Interval, _stop.Token).ConfigureAwait(false); }
            catch (OperationCanceledException) { return; }
        }
    }

    private static async Task<List<ServiceRecord>?> QueryAsync(CancellationToken cancellation)
    {
        var start = new ProcessStartInfo(AppPaths.CliPath())
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        start.ArgumentList.Add("discover");
        start.ArgumentList.Add("--json");
        start.ArgumentList.Add("--timeout-ms");
        start.ArgumentList.Add("2500");
        using var process = Process.Start(start);
        if (process is null) throw new IOException(L.Get("Could not start AirFlash discovery."));
        var output = await process.StandardOutput.ReadToEndAsync(cancellation).ConfigureAwait(false);
        var errors = await process.StandardError.ReadToEndAsync(cancellation).ConfigureAwait(false);
        await process.WaitForExitAsync(cancellation).ConfigureAwait(false);
        if (process.ExitCode != 0)
            throw new IOException(errors.Length > 0 ? errors : L.Get("Receiver discovery failed."));
        return Parse(output);
    }

    /// <summary>Maps the CLI's discovery JSON onto the shared service record model.</summary>
    public static List<ServiceRecord>? Parse(string json)
    {
        using var document = JsonDocument.Parse(json);
        var records = new List<ServiceRecord>();
        foreach (var element in document.RootElement.EnumerateArray())
        {
            var instance = element.TryGetProperty("instance", out var name) ? name.GetString() ?? "" : "";
            var host = element.TryGetProperty("host", out var hostName) ? hostName.GetString() ?? "" : "";
            var port = element.TryGetProperty("port", out var portValue) ? portValue.GetInt32() : 7000;
            var address = element.TryGetProperty("addresses", out var addresses) && addresses.GetArrayLength() > 0
                ? addresses[0].GetString() ?? host
                : host;
            var txt = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
            if (element.TryGetProperty("txt", out var fields) && fields.ValueKind == JsonValueKind.Object)
            {
                foreach (var field in fields.EnumerateObject())
                    txt[field.Name] = field.Value.ValueKind == JsonValueKind.String ? field.Value.GetString() ?? "" : field.Value.ToString();
            }
            if (instance.Length == 0 || address.Length == 0) continue;
            records.Add(new ServiceRecord(instance, AirPlayService(instance), address, port, txt));
        }
        return records;
    }

    private static string AirPlayService(string instance) =>
        instance.Contains("_raop", StringComparison.Ordinal) ? "_raop._tcp.local" : "_airplay._tcp.local";

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _stop.Cancel();
        _stop.Dispose();
        _gate.Dispose();
        _timer.Dispose();
        GC.SuppressFinalize(this);
    }
}

/// <summary>
/// Linux audio: the sender runs in a user session and the engine owns capture,
/// so local mute is a no-op and endpoint selection is replaced by the streaming
/// source choice. System capture is not implemented yet, which the settings page
/// states explicitly.
/// </summary>
public sealed class LinuxAudioService : IAudioService
{
#pragma warning disable CS0067 // No mixer session to notify on Linux yet.
    public event Action? EndpointsChanged;
#pragma warning restore CS0067
    public Task<IReadOnlyList<AudioEndpoint>> GetEndpointsAsync() => Task.FromResult<IReadOnlyList<AudioEndpoint>>([]);
    public Task<string?> GetDefaultEndpointIdAsync() => Task.FromResult<string?>(null);
    public Task MuteAsync(string? endpointId) => Task.CompletedTask;
    public Task RestoreAsync() => Task.CompletedTask;
}

/// <summary>Starts the app with the session; the Windows build uses the Run key.</summary>
public interface IAutostart
{
    void Set(bool enabled);
    Task SetAsync(bool enabled) => Task.Run(() => Set(enabled));
}

/// <summary>Autostart through the freedesktop.org entry in ~/.config/autostart.</summary>
public sealed class LinuxAutostart : IAutostart
{
    private static string EntryPath => Path.Combine(
        Environment.GetEnvironmentVariable("XDG_CONFIG_HOME") is { Length: > 0 } config ? config
            : Path.Combine(Environment.GetEnvironmentVariable("HOME") ?? ".", ".config"),
        "autostart", "airflash-ui.desktop");

    public void Set(bool enabled)
    {
        try
        {
            if (!enabled)
            {
                if (File.Exists(EntryPath)) File.Delete(EntryPath);
                return;
            }
            Directory.CreateDirectory(Path.GetDirectoryName(EntryPath)!);
            var executable = Environment.ProcessPath ?? "AirFlash.UI";
            File.WriteAllText(EntryPath, string.Create(CultureInfo.InvariantCulture,
                $"[Desktop Entry]\nType=Application\nName=AirFlash\nExec={executable} --tray\nIcon=airflash\nTerminal=false\nCategories=AudioVideo;Audio;\nX-GNOME-Autostart-enabled=true\n"));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            AppPaths.Log($"Autostart not updated: {error.Message}");
        }
    }
}

/// <summary>Single instance through a lock file in the per-user runtime directory.</summary>
public sealed class SingleInstance : IDisposable
{
    private readonly string _lockPath = Path.Combine(AppPaths.StateDirectory, "ui.lock");
    private readonly FileStream? _lock;
    private readonly CancellationTokenSource _stop = new();
    private Task? _watcher;
    private bool _disposed;
    public bool IsOwner { get; }

    public SingleInstance()
    {
        try
        {
            Directory.CreateDirectory(AppPaths.StateDirectory);
            _lock = new FileStream(_lockPath, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
            IsOwner = true;
        }
        catch (IOException)
        {
            IsOwner = false;
        }
        catch (UnauthorizedAccessException)
        {
            IsOwner = false;
        }
    }

    public async Task NotifyExistingAsync()
    {
        // Waking the running instance is cosmetic; failures are ignored.
        try
        {
            var signal = Path.Combine(AppPaths.StateDirectory, "ui.activate");
            Directory.CreateDirectory(AppPaths.StateDirectory);
            await File.WriteAllTextAsync(signal, DateTime.UtcNow.ToString("O"));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            AppPaths.Log(error.ToString());
        }
    }

    public void Listen(Action activate)
    {
        if (!IsOwner) return;
        _watcher = Task.Run(async () =>
        {
            var seen = DateTime.MinValue;
            var signal = Path.Combine(AppPaths.StateDirectory, "ui.activate");
            while (!_stop.IsCancellationRequested)
            {
                try
                {
                    if (File.Exists(signal) && File.GetLastWriteTimeUtc(signal) != seen)
                    {
                        seen = File.GetLastWriteTimeUtc(signal);
                        activate();
                    }
                }
                catch (Exception error) when (error is IOException or UnauthorizedAccessException) { }
                try { await Task.Delay(TimeSpan.FromMilliseconds(700), _stop.Token).ConfigureAwait(false); }
                catch (OperationCanceledException) { return; }
            }
        });
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _stop.Cancel();
        try { _lock?.Dispose(); if (IsOwner && File.Exists(_lockPath)) File.Delete(_lockPath); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { }
        _stop.Dispose();
        GC.SuppressFinalize(this);
    }
}
