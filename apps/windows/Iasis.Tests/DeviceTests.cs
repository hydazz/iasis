using System.Runtime.CompilerServices;
using System.Text.Json;
using Iasis.Agent;
using Xunit;

namespace Iasis.Tests;

public class DeviceTests
{
    private static Device DeviceOn(string serial, int number, string? ecid = null, [CallerFilePath] string path = "")
    {
        var fixture = Path.Combine(Path.GetDirectoryName(path)!, "../../../crates/api/fixtures/state.json");
        var state = JsonSerializer.Deserialize(File.ReadAllText(fixture), AgentJson.Default.AgentState)!;
        var hub = state.Hubs[0] with { Serial = serial, Session = 3 };
        return new(hub, hub.Ports[0] with { Number = number, Generation = 7, Ecid = ecid }, null);
    }

    [Fact]
    public void ConfirmationAndResultLabelsKeepTheSameAttachmentsInWireOrder()
    {
        var a = DeviceOn("aaa", 1, "1234");
        var b = DeviceOn("bbb", 1, "5678");
        var c = DeviceOn("aaa", 2);
        var targets = DeviceActionTargets.For([a, b, c], DeviceAction.Restore)!;
        var request = Assert.IsType<RestoreRequest>(targets.Request(DeviceAction.Restore, latest: true));
        Assert.Equal([a, c, b], targets.Devices);
        Assert.Equal(["aaa", "bbb"], request.Hubs.Select(h => h.Hub));
        Assert.Equal(3u, request.Hubs[0].Session);
        Assert.Equal([new Target(1, 7, "1234"), new Target(2, 7, null)], request.Hubs[0].Targets);
        Assert.True(request.Latest);
        Assert.False(a.SameAttachment(a with { Port = a.Port with { Generation = 8 } }));
        Assert.False(a.SameAttachment(a with { Port = a.Port with { Ecid = "4321" } }));
        Assert.False(a.SameAttachment(a with { Hub = a.Hub with { Session = 4 } }));
        Assert.True(c.SameAttachment(c with { Port = c.Port with { Ecid = "4321" } }));
    }

    [Fact]
    public void SignalsUseEligiblePortsAndRestoreNeedsEveryTarget()
    {
        var a = DeviceOn("aaa", 1);
        var b = a with
        {
            Port = a.Port with { Number = 2, Dfu = true },
            Needs = new("driver", "Driver", null, "Install the driver."),
        };
        var signal = DeviceActionTargets.For([a, b], DeviceAction.Dfu)!;
        Assert.Equal([2], signal.Hubs.Single().Targets.Select(t => t.Port));
        Assert.Null(DeviceActionTargets.For([a, b], DeviceAction.Restore));
        Assert.Null(DeviceActionTargets.For([a, b], DeviceAction.Revive));
        Assert.Null(DeviceActionTargets.For([], DeviceAction.Restore));
        Assert.Null(DeviceActionTargets.For([a with { Hub = a.Hub with { Session = null } }], DeviceAction.Restart));
    }

    [Fact]
    public void EraseRequiresAPlanForEachPortAndDisplaysImageChoices()
    {
        var a = DeviceOn("aaa", 1);
        var targets = DeviceActionTargets.For([a], DeviceAction.Restore)!;
        var held = new Release("iOS", "26.10", "24A99", 123);
        var latest = held with { Build = "24A101" };
        var review = targets.Review([new("aaa", 1, new(latest, held, null))]);
        Assert.True(review.NewerAvailable);
        Assert.False(review.Blocked);
        Assert.Contains("aaa", review.Summary);
        Assert.Contains("port 1", review.Summary);
        Assert.Contains("24A99", review.Summary);
        Assert.Contains("24A101", review.Summary);
        Assert.False(targets.Review([new("aaa", 1, new(held, latest, null))]).NewerAvailable);
        Assert.Throws<InvalidDataException>(() => targets.Review([new("bbb", 1, null)]));
        var unknown = targets.Review([new("aaa", 1, null)]);
        Assert.False(unknown.Blocked);
        var blocked = targets.Review([new("aaa", 1, new(null, null, "catalog offline"))]);
        Assert.True(blocked.Blocked);
        Assert.Contains("catalog offline", blocked.Summary);
    }
}
