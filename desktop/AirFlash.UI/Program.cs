using AirFlash.UI.Services;
using Avalonia;

namespace AirFlash.UI;

internal static class Program
{
    /// <summary>
    /// Classic desktop entry point; the tray keeps the app alive when the window
    /// closes. The single-instance lock is taken before Avalonia starts so a
    /// second launch exits quietly instead of tearing down a live dispatcher.
    /// </summary>
    [STAThread]
    public static async Task<int> Main(string[] args)
    {
        using var instance = new SingleInstance();
        if (!instance.IsOwner)
        {
            // Another panel is already running; ask it to show itself.
            await instance.NotifyExistingAsync();
            return 0;
        }
        AppHost.Instance = instance;
        try
        {
            return BuildAvaloniaApp().StartWithClassicDesktopLifetime(args);
        }
        catch (Exception error)
        {
            // A headless log is the only trace when there is no terminal attached.
            AppPaths.Log($"Fatal: {error}");
            return 1;
        }
    }

    public static AppBuilder BuildAvaloniaApp() =>
        AppBuilder.Configure<App>().UsePlatformDetect().LogToTrace();
}

/// <summary>Process-wide services created before the UI framework exists.</summary>
internal static class AppHost
{
    public static SingleInstance? Instance { get; set; }
}
