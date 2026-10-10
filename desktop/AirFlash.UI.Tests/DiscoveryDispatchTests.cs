using AirFlash.Core;
using AirFlash.UI.ViewModels;
using Avalonia.Controls;
using Avalonia.Headless.XUnit;
using Avalonia.Threading;
using Xunit;

namespace AirFlash.UI.Tests;

/// <summary>
/// Discovery is raised on a worker thread and marshalled onto the UI thread, so
/// the reconcile must be awaited rather than blocked on: a blocking Wait plus
/// GetAwaiter().GetResult() on the UI thread deadlocks the dispatcher, because
/// the awaits inside capture the UI SynchronizationContext and queue their
/// continuation to the very thread that waits for them. The symptom is subtle —
/// the panel opens and keeps saying "Searching for receivers" — so it is pinned
/// down here on a real dispatcher.
/// </summary>
public sealed class DiscoveryDispatchTests
{
    private sealed class FakeDiscovery(IReadOnlyList<Receiver> receivers) : IDiscoveryService
    {
        public event Action<IReadOnlyList<Receiver>>? Changed;
#pragma warning disable CS0067 // Part of IDiscoveryService; nothing raises it here.
        public event Action<string>? Failed;
#pragma warning restore CS0067
        public void SetInterface(string id) { }
        public void Start() => Changed?.Invoke(receivers);
        public void Dispose() => Changed = null;
    }

    private sealed class TempStore(string path) : ISettingsStore
    {
        public AppSettings Load() => File.Exists(path)
            ? System.Text.Json.JsonSerializer.Deserialize<AppSettings>(File.ReadAllText(path), AppSettings.JsonOptions) ?? new()
            : new();
        public void Save(AppSettings settings)
        {
            Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
            File.WriteAllText(path, System.Text.Json.JsonSerializer.Serialize(settings, AppSettings.JsonOptions));
        }
        // Deliberately asynchronous, like the real store: this is what makes a
        // blocking reconcile deadlock instead of merely being slow.
        public Task SaveReceiverIdentityAsync(AppSettings settings, bool backup) => Task.Run(() => Save(settings));
    }

    private sealed class QuietAutostart : Services.IAutostart
    {
        public void Set(bool enabled) { }
    }

    private sealed class QuietAudio : IAudioService
    {
#pragma warning disable CS0067 // No mixer session on Linux; part of the interface.
        public event Action? EndpointsChanged;
#pragma warning restore CS0067
        public Task<IReadOnlyList<AudioEndpoint>> GetEndpointsAsync() => Task.FromResult<IReadOnlyList<AudioEndpoint>>([]);
        public Task<string?> GetDefaultEndpointIdAsync() => Task.FromResult<string?>(null);
        public Task MuteAsync(string? endpointId) => Task.CompletedTask;
        public Task RestoreAsync() => Task.CompletedTask;
    }

    private sealed class NoEngine : IEngineFactory
    {
        public IEngineConnection Open() => throw new NotSupportedException("no engine in this test");
    }

    [AvaloniaFact]
    public async Task Discovered_receivers_reach_the_panel_without_deadlocking_the_dispatcher()
    {
        var directory = Path.Combine(Path.GetTempPath(), "airflash-dispatch-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var receivers = new[] { new Receiver("abc123", "Test Pod", "10.0.0.9", 7000) { Model = "AudioAccessory5,1" } };
            var viewModel = new AppViewModel(
                new TempStore(Path.Combine(directory, "settings.json")),
                new FakeDiscovery(receivers),
                new QuietAutostart(),
                new QuietAudio(),
                new NoEngine());
            viewModel.Start();

            // Wait on a thread-pool timer so a deadlocked dispatcher still fails
            // the test instead of hanging the suite.
            var deadline = DateTime.UtcNow.AddSeconds(15);
            while (viewModel.Receivers.Count == 0 && DateTime.UtcNow < deadline)
            {
                await Task.Delay(50).ConfigureAwait(false);
                Dispatcher.UIThread.RunJobs();
            }
            await Task.CompletedTask;

            Assert.NotEmpty(viewModel.Receivers);
            Assert.Contains(viewModel.Receivers, receiver => receiver.Name == "Test Pod");
        }
        finally { Directory.Delete(directory, true); }
    }

    [AvaloniaFact]
    public void The_panel_window_builds_its_tree()
    {
        // A headless smoke test of the ported AXAML: resource lookups, styles and
        // the localized markup extension all have to resolve at runtime.
        var window = new Views.MainWindow { DataContext = new ViewModels.AppViewModel(
            new TempStore(Path.Combine(Path.GetTempPath(), "airflash-window-" + Guid.NewGuid().ToString("N") + ".json")),
            new FakeDiscovery([]),
            new QuietAutostart(),
            new QuietAudio(),
            new NoEngine()) };
        window.Show();
        Dispatcher.UIThread.RunJobs();
        Assert.NotNull(window.Content);
        window.Close();
    }
}
