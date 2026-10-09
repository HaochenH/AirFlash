using System.Net;
using AirFlash.Core;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Interactivity;

namespace AirFlash.UI.Views;

/// <summary>Adds a receiver by address when discovery cannot see it.</summary>
public partial class ManualReceiverDialog : Window
{
    public ManualReceiverDialog() => InitializeComponent();

    public bool Accepted { get; private set; }
    public string ReceiverName => NameBox.Text ?? "";
    public string Host => HostBox.Text?.Trim() ?? "";
    public int Port => int.TryParse(PortBox.Text, out var port) ? port : 7000;

    private void OnAdd(object? sender, RoutedEventArgs args)
    {
        if (!IPAddress.TryParse(Host, out var address) || address.AddressFamily != System.Net.Sockets.AddressFamily.InterNetwork)
        {
            ErrorText.Text = L.Get("Enter an IPv4 address, for example 192.168.1.42.");
            return;
        }
        if (Port is < 1 or > 65535)
        {
            ErrorText.Text = L.Get("The port must be between 1 and 65535.");
            return;
        }
        Accepted = true;
        Close(true);
    }

    private void OnCancel(object? sender, RoutedEventArgs args) => Close(false);
}
