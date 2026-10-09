using AirFlash.Core;
using AirFlash.UI.Services;
using Xunit;

namespace AirFlash.UI.Tests;

/// <summary>
/// Discovery reuses the Rust CLI's JSON output; these tests pin the mapping from
/// that JSON to the shared service-record model, including stereo merging.
/// </summary>
public sealed class DiscoveryMappingTests
{
    [Fact]
    public void Parses_receivers_from_cli_json()
    {
        var json = """
        [
          {"instance":"Study._airplay._tcp.local","service":"_airplay._tcp.local","host":"study-pod.local",
           "port":7000,"addresses":["192.168.1.42"],
           "txt":{"deviceid":"AA:BB:CC:DD:EE:01","model":"AudioAccessory5,1"}},
          {"instance":"Kitchen._raop._tcp.local","service":"_raop._tcp.local","host":"kitchen-pod.local",
           "port":7000,"addresses":["192.168.1.43"],"txt":{"deviceid":"AA:BB:CC:DD:EE:02"}}
        ]
        """;
        var records = LinuxDiscovery.Parse(json) ?? throw new InvalidOperationException("no records");
        Assert.Equal(2, records.Count);
        var study = records[0];
        Assert.Equal("Study._airplay._tcp.local", study.Instance);
        Assert.Equal("192.168.1.42", study.Address);
        Assert.Equal(7000, study.Port);
        Assert.Equal("AA:BB:CC:DD:EE:01", study.Txt["deviceid"]);
        // The raop record keeps its service type so codec filtering still works.
        Assert.StartsWith("_raop", records[1].ServiceType, StringComparison.Ordinal);
    }

    [Fact]
    public void Aggregates_a_stereo_pair_like_the_windows_discovery()
    {
        var json = """
        [
          {"instance":"Left._airplay._tcp.local","host":"left.local","port":7000,"addresses":["10.0.0.2"],
           "txt":{"deviceid":"AA:00","tsid":"PAIR1","igl":"1","model":"AudioAccessory5,1"}},
          {"instance":"Right._airplay._tcp.local","host":"right.local","port":7000,"addresses":["10.0.0.3"],
           "txt":{"deviceid":"AA:11","tsid":"PAIR1","igl":"0","model":"AudioAccessory5,1"}}
        ]
        """;
        var records = LinuxDiscovery.Parse(json)!;
        var receivers = ReceiverAggregator.Build(records);
        var stereo = Assert.Single(receivers);
        Assert.True(stereo.IsGroup);
        Assert.Equal(2, stereo.Members.Length);
        Assert.True(stereo.Complete, "both members resolved");
        Assert.Equal("PAIR1", stereo.StereoId);
    }

    [Fact]
    public void Ignores_records_without_an_instance_and_falls_back_to_the_host()
    {
        var json = """
        [{"instance":"","host":"h.local","port":7000,"addresses":["10.0.0.1"],"txt":{}},
         {"instance":"Nameless._airplay._tcp.local","host":"h.local","port":7000,"addresses":[],"txt":{}},
         {"instance":"Named._airplay._tcp.local","host":"h.local","port":7000,"addresses":["10.0.0.9"],"txt":{}}]
        """;
        var records = LinuxDiscovery.Parse(json)!;
        Assert.Equal(2, records.Count);
        // Without addresses the host stays, and the session resolves it when connecting.
        Assert.Equal("h.local", records[0].Address);
        Assert.Equal("10.0.0.9", records[1].Address);
    }

    [Fact]
    public void Invalid_json_is_an_error_not_a_crash()
    {
        Assert.ThrowsAny<System.Text.Json.JsonException>(() => LinuxDiscovery.Parse("not json"));
    }
}

/// <summary>Linux autostart writes and removes a freedesktop.org entry.</summary>
public sealed class AutostartTests
{
    [Fact]
    public void Enable_and_disable_round_trip()
    {
        var home = Path.Combine(Path.GetTempPath(), "airflash-ui-test-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(home);
        var previousConfig = Environment.GetEnvironmentVariable("XDG_CONFIG_HOME");
        var previousHome = Environment.GetEnvironmentVariable("HOME");
        try
        {
            Environment.SetEnvironmentVariable("XDG_CONFIG_HOME", home);
            Environment.SetEnvironmentVariable("HOME", home);
            var entry = Path.Combine(home, "autostart", "airflash-ui.desktop");
            var autostart = new LinuxAutostart();
            autostart.Set(true);
            Assert.True(File.Exists(entry), "enabling autostart writes the entry");
            var text = File.ReadAllText(entry);
            Assert.Contains("[Desktop Entry]", text, StringComparison.Ordinal);
            Assert.Contains("Type=Application", text, StringComparison.Ordinal);
            Assert.Contains("X-GNOME-Autostart-enabled=true", text, StringComparison.Ordinal);
            autostart.Set(false);
            Assert.False(File.Exists(entry), "disabling autostart removes the entry");
        }
        finally
        {
            Environment.SetEnvironmentVariable("XDG_CONFIG_HOME", previousConfig);
            Environment.SetEnvironmentVariable("HOME", previousHome);
            Directory.Delete(home, true);
        }
    }
}

/// <summary>The engine is resolved from the environment, the app directory, or PATH.</summary>
public sealed class EngineResolutionTests
{
    [Fact]
    public void Explicit_override_wins()
    {
        var previous = Environment.GetEnvironmentVariable("AIRFLASH_ENGINE");
        var path = Path.Combine(Path.GetTempPath(), "airflash-engine-" + Guid.NewGuid().ToString("N"));
        File.WriteAllText(path, "#!/bin/sh\nexit 0\n");
        try
        {
            Environment.SetEnvironmentVariable("AIRFLASH_ENGINE", path);
            Assert.Equal(Path.GetFullPath(path), AppPaths.EnginePath());
        }
        finally
        {
            Environment.SetEnvironmentVariable("AIRFLASH_ENGINE", previous);
            File.Delete(path);
        }
    }

    [Fact]
    public void Missing_engine_is_an_explicit_error()
    {
        var previous = Environment.GetEnvironmentVariable("AIRFLASH_ENGINE");
        try
        {
            Environment.SetEnvironmentVariable("AIRFLASH_ENGINE", Path.Combine(Path.GetTempPath(), "does-not-exist-airflash-engine"));
            Environment.SetEnvironmentVariable("PATH", "");
            var error = Assert.Throws<FileNotFoundException>(() => AppPaths.EnginePath());
            Assert.Contains("AIRFLASH_ENGINE", error.Message, StringComparison.Ordinal);
        }
        finally
        {
            Environment.SetEnvironmentVariable("AIRFLASH_ENGINE", previous);
        }
    }
}

/// <summary>Linux first run must not select the Windows-only capture source.</summary>
public sealed class SettingsDefaultsTests
{
    [Fact]
    public void Streaming_source_defaults_are_valid_on_every_platform()
    {
        Assert.Contains("loopback", AppSettings.StreamSources, StringComparer.Ordinal);
        Assert.Contains("simulated", AppSettings.StreamSources, StringComparer.Ordinal);
        Assert.Contains("file", AppSettings.StreamSources, StringComparer.Ordinal);
        var settings = new AppSettings { StreamSource = "simulated" };
        Assert.Null(settings.Validate());
        var file = new AppSettings { StreamSource = "file", StreamFilePath = "/tmp/audio.wav" };
        Assert.Null(file.Validate());
        var missing = new AppSettings { StreamSource = "file" };
        Assert.NotNull(missing.Validate());
        Assert.False(new AppSettings().Validate() is null && new AppSettings().StreamSource != "loopback",
            "the stored default stays loopback so Windows behavior is unchanged");
    }

    [Fact]
    public void Settings_round_trip_through_json_keeps_the_source()
    {
        var settings = new AppSettings { StreamSource = "file", StreamFilePath = "/tmp/one.wav" };
        var clone = settings.Clone();
        Assert.Equal("file", clone.StreamSource);
        Assert.Equal("/tmp/one.wav", clone.StreamFilePath);
    }
}
