using AirFlash.Core;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Interactivity;

namespace AirFlash.UI.Views;

/// <summary>Asks for the PIN shown on the receiver; the value is read by the caller.</summary>
public partial class PinDialog : Window
{
    public PinDialog() => InitializeComponent();

    public string Prompt
    {
        get => PromptText.Text ?? "";
        set => PromptText.Text = value;
    }
    public string Pin
    {
        get => PinBox.Text ?? "";
        set => PinBox.Text = value;
    }

    private void OnOk(object? sender, RoutedEventArgs args)
    {
        var pin = Pin;
        if (pin.Length is < 4 or > 8 || !pin.All(char.IsAsciiDigit))
        {
            Error.Text = L.Get("The PIN must contain 4 to 8 digits.");
            return;
        }
        Close(true);
    }

    private void OnCancel(object? sender, RoutedEventArgs args) => Close(false);
}
