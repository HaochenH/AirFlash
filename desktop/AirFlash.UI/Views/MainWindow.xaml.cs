using AirFlash.Core;
using AirFlash.UI.ViewModels;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;

namespace AirFlash.UI.Views;

public partial class MainWindow : Window
{
    private SettingsWindow? _settings;
    private PinDialog? _pin;
    private ManualReceiverDialog? _manual;

    public MainWindow() => InitializeComponent();

    private AppViewModel? Model => DataContext as AppViewModel;

    protected override void OnOpened(EventArgs args)
    {
        base.OnOpened(args);
        if (Model is { } model) model.SetMonitorVisible(false);
    }

    private void OnSettings(object? sender, RoutedEventArgs args)
    {
        if (Model is not { } model) return;
        _settings ??= new SettingsWindow { DataContext = new SettingsViewModel(model) };
        _settings.Show();
        _settings.Activate();
    }

    private void OnQuit(object? sender, RoutedEventArgs args)
    {
        if (Application.Current is App app) app.Quit();
        else Close();
    }

    /// <summary>Raised by the settings window when it needs a PIN for pairing.</summary>
    public void RequestPair(Receiver receiver)
    {
        if (Model is not { } model) return;
        _pin ??= new PinDialog();
        _pin.Prompt = string.Format(Core.L.Get("Enter the AirPlay PIN shown on {0}"), receiver.Name);
        _pin.Pin = "";
        _pin.ShowDialog(this);
        var pin = _pin.Pin;
        if (!string.IsNullOrWhiteSpace(pin))
        {
            _ = model.Session.PairAsync(receiver, model.Settings.Clone(), (_, _) => Task.FromResult<string?>(pin));
        }
    }

    public void RequestManualReceiver()
    {
        if (Model is not { } model) return;
        _manual ??= new ManualReceiverDialog();
        _manual.ShowDialog(this);
        if (_manual.Accepted)
        {
            var settings = new SettingsViewModel(model);
            settings.AddManual(_manual.ReceiverName, _manual.Host, _manual.Port);
        }
    }

    protected override void OnKeyDown(KeyEventArgs args)
    {
        base.OnKeyDown(args);
        if (args.Key == Key.Escape) Close();
    }
}
