using AirFlash.Core;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Interactivity;

namespace AirFlash.UI.Views;

/// <summary>Shows the bundled third-party notices; the same file ships with the Windows build.</summary>
public partial class LicensesWindow : Window
{
    public LicensesWindow()
    {
        InitializeComponent();
        Notice.Text = L.Get(
            "AirFlash uses RustCrypto (sha2, hkdf, ChaCha20-Poly1305), dalek (Ed25519, X25519), "
            + "num-bigint, Rubato, alac-encoder and hound under MIT or Apache-2.0; libc under MIT OR Apache-2.0; "
            + "and Avalonia UI under the MIT license. See the repository's LICENSE-GPLv3, "
            + "LICENSE-COMMERCIAL.md and THIRD-PARTY-NOTICES.md for the full texts.");
    }

    private void OnClose(object? sender, RoutedEventArgs args) => Close();
}
