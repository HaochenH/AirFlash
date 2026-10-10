using Avalonia;
using Avalonia.Headless;

[assembly: AvaloniaTestApplication(typeof(AirFlash.UI.Tests.HeadlessApp))]

namespace AirFlash.UI.Tests;

/// <summary>
/// Runs the real application class headlessly so tests resolve the shipped
/// brushes, styles and converters instead of a stub. App only builds services
/// for a classic desktop lifetime, which the headless test lifetime is not, so
/// the tests get the resources without the tray, the window or an engine.
/// </summary>
public sealed class HeadlessApp : App
{
    public static AppBuilder BuildAvaloniaApp() => AppBuilder
        .Configure<HeadlessApp>()
        .UseHeadless(new AvaloniaHeadlessPlatformOptions { UseHeadlessDrawing = true });
}
