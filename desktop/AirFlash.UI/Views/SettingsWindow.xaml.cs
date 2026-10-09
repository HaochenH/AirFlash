using AirFlash.Core;
using AirFlash.UI.ViewModels;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Interactivity;
using Avalonia.Threading;
using Avalonia.Platform.Storage;

namespace AirFlash.UI.Views;

public partial class SettingsWindow : Window
{

    public SettingsWindow() => InitializeComponent();

    private SettingsViewModel? Model => DataContext as SettingsViewModel;

    protected override void OnOpened(EventArgs args)
    {
        base.OnOpened(args);
        if (Model is { } model)
        {
            model.CloseRequested += OnCloseRequested;
            model.AddReceiverRequested += OnAddReceiver;
            model.PairRequested += OnPair;
            model.LicensesRequested += OnLicenses;
            if (Navigation is { } navigation)
            {
                navigation.SelectionChanged += (_, _) =>
                {
                    if (Model is { } current) current.App.SetMonitorVisible(current.SelectedPage == SettingsViewModel.MonitorPage);
                };
            }
            ShowPage(Model.SelectedPage);
        }
    }

    private void ShowPage(int page)
    {
        if (PageContainer is null) return;
        var resource = page switch
        {
            0 => "Page0",
            1 => "Page1",
            2 => "Page2",
            3 => "Page3",
            4 => "Page4",
            5 => "Page5",
            6 => "Page6",
            _ => "Page7",
        };
        if (Resources.TryGetResource(resource, ActualThemeVariant, out var template) && template is Avalonia.Markup.Xaml.Templates.DataTemplate data)
        {
            PageContainer.Content = data.Build(Model);
        }
    }

    private void OnCloseRequested(bool saveAndClose)
    {
        if (saveAndClose && Model is { } model && model.HasChanges)
        {
            _ = model.ApplyAsync().ContinueWith(_ => Dispatcher.UIThread.Post(Close), TaskScheduler.Default);
            return;
        }
        Close();
    }

    private void OnAddReceiver()
    {
        if (Owner is MainWindow main && Model is { } model)
        {
            main.RequestManualReceiver();
        }
    }

    private void OnPair(Receiver receiver)
    {
        if (Owner is MainWindow main) main.RequestPair(receiver);
    }

    private void OnLicenses() => new LicensesWindow().ShowDialog(this);

    private async void OnChooseFile(object? sender, RoutedEventArgs args)
    {
        if (Model is not { } model) return;
        var files = await StorageProvider.OpenFilePickerAsync(new FilePickerOpenOptions
        {
            Title = AirFlash.Core.L.Get("Choose a WAV file"),
            AllowMultiple = false,
            FileTypeFilter = new[]
            {
                FilePickerFileTypes.All,
                new FilePickerFileType(L.Get("WAV audio")) { Patterns = ["*.wav"] },
            },
        });
        if (files.Count == 0) return;
        var path = files[0].Path.LocalPath;
        model.Draft.StreamSource = "file";
        model.Draft.StreamFilePath = path;
        model.App.Sources.SetFile(path);
    }

    protected override void OnClosed(EventArgs args)
    {
        if (Model is { } model)
        {
            model.CloseRequested -= OnCloseRequested;
            model.AddReceiverRequested -= OnAddReceiver;
            model.PairRequested -= OnPair;
            model.LicensesRequested -= OnLicenses;
            model.App.SetMonitorVisible(false);
            model.Dispose();
        }
        base.OnClosed(args);
    }
}
