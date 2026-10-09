using Avalonia;

namespace AirFlash.UI;

internal static class Program
{
    /// <summary>Classic desktop entry point; the tray keeps the app alive when the window closes.</summary>
    [STAThread]
    public static void Main(string[] args)
    {
        try
        {
            BuildAvaloniaApp().StartWithClassicDesktopLifetime(args);
        }
        catch (Exception error)
        {
            // A headless log is the only trace when there is no terminal attached.
            Services.AppPaths.Log($"Fatal: {error}");
            throw;
        }
    }

    public static AppBuilder BuildAvaloniaApp() =>
        AppBuilder.Configure<App>().UsePlatformDetect().LogToTrace();
}
