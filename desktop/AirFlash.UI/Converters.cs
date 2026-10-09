using System.Globalization;
using Avalonia.Data;
using Avalonia.Data.Converters;
using Avalonia.Markup.Xaml;
using Avalonia.Media;

namespace AirFlash.UI;

/// <summary>Localized markup extension: {loc:Text Settings} reads the current language.</summary>
public sealed class TextExtension(string key) : MarkupExtension
{
    public string Key { get; init; } = key;
    public override object ProvideValue(IServiceProvider serviceProvider) => AirFlash.Core.L.Get(Key);
}

/// <summary>Converts a non-empty string to Visible so hints stay collapsed when there is nothing to say.</summary>
public sealed class NonEmptyConverter : IValueConverter
{
    public static readonly NonEmptyConverter Instance = new();
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        value is string text && text.Length > 0;
    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        throw new NotSupportedException();
}

/// <summary>Boolean to visibility for the hints that only appear when something is wrong.</summary>
public sealed class BoolToVisibilityConverter : IValueConverter
{
    public static readonly BoolToVisibilityConverter Instance = new();
    public bool Invert { get; init; }
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        var visible = value switch { true => true, false => false, _ => false };
        return Invert ? !visible : visible;
    }
    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        value is true;
}

/// <summary>Equality against a converter parameter, used by the settings radio buttons.</summary>
public sealed class EqualityConverter : IValueConverter
{
    public static readonly EqualityConverter Instance = new();
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        Equals(value?.ToString(), parameter?.ToString());
    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        value is true ? parameter : BindingOperations.DoNothing;
}
