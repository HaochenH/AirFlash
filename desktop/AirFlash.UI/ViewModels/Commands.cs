using System.Windows.Input;
using AirFlash.Core;

namespace AirFlash.UI.ViewModels;

public sealed class RelayCommand(Action execute, Func<bool>? canExecute = null) : ICommand
{
    public event EventHandler? CanExecuteChanged;
    public bool CanExecute(object? parameter) => canExecute?.Invoke() ?? true;
    public void Execute(object? parameter) => execute();
    public void Refresh() => CanExecuteChanged?.Invoke(this, EventArgs.Empty);
}

public sealed class AsyncCommand(Func<Task> execute, Action<Exception> onError, Func<bool>? canExecute = null) : ICommand
{
    private bool _busy;
    public event EventHandler? CanExecuteChanged;
    public bool CanExecute(object? parameter) => !_busy && (canExecute?.Invoke() ?? true);
    public async void Execute(object? parameter)
    {
        if (!CanExecute(parameter)) return;
        _busy = true; Refresh();
        try { await execute(); }
        catch (Exception error) { onError(error); }
        finally { _busy = false; Refresh(); }
    }
    public void Refresh() => CanExecuteChanged?.Invoke(this, EventArgs.Empty);
}

public sealed record Choice(object? Value, string Label);
public static class Choices
{
    public static Choice[] Languages => [new("system", L.Get("System default")), new("zh", "简体中文"), new("en", "English")];
    public static Choice[] Themes => [new("system", L.Get("System default")), new("dark", L.Get("Dark")), new("light", L.Get("Light"))];
    public static Choice[] Latencies => [new("realtime", L.Get("Real-time · 120 ms")), new("normal", L.Get("Normal · 200 ms")), new("buffered", L.Get("Buffered · 500 ms")), new("custom", L.Get("Custom"))];
    public static Choice[] ReceiverLatencies => [new(null, L.Get("Use global setting")), .. Latencies];
    public static Choice[] AutoConnect => [new(null, L.Get("Use global setting")), new(true, L.Get("On")), new(false, L.Get("Off"))];
    /// <summary>Streaming inputs available on Linux: system capture is not implemented yet.</summary>
    public static Choice[] StreamSources =>
    [
        new("simulated", L.Get("Simulated test signal")),
        new("file", L.Get("WAV file…")),
        new("loopback", L.Get("System audio (not available on Linux)")),
    ];
}

/// <summary>Replaces the Windows endpoint catalog: Linux chooses an input, not an audio device.</summary>
public sealed class AudioSourceCatalog : ObservableObject
{
    public Choice[] Sources => Choices.StreamSources;
    public bool IsFilePicked { get; private set; }
    public string FileName { get; private set; } = "";
    public void SetFile(string path)
    {
        FileName = Path.GetFileName(path);
        IsFilePicked = true;
        Notify(nameof(FileName));
        Notify(nameof(HasFile));
    }
    public void ClearFile()
    {
        FileName = "";
        IsFilePicked = false;
        Notify(nameof(FileName));
        Notify(nameof(HasFile));
    }
    public bool HasFile => FileName.Length > 0;
    private bool _Loading;
    public bool Loading { get => _Loading; private set { if (Set(ref _Loading, value)) Notify(nameof(LoadingText)); } }
    public string LoadingText => Loading ? L.Get("Reading audio devices…") : "";
    public Task RefreshAsync() { Loading = false; return Task.CompletedTask; }
}
