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
    public void RestorePowerShowsOneCurrentWarningPerSelectedHub()
    {
        var first = DeviceOn("aaa", 1);
        var second = DeviceOn("bbb", 1);
        var last = DeviceOn("ccc", 1);
        var targets = DeviceActionTargets.For([first, second, first with { Port = first.Port with { Number = 2 } }, last], DeviceAction.Restore)!;
        var warned = first.Hub with { Name = "Bench", RestorePowerWarning = "First warning." };
        var other = last.Hub with { Name = "", RestorePowerWarning = "Second warning." };
        var unrelated = warned with { Serial = "ddd" };
        var reconnected = warned with { Session = 4 };
        Assert.Null(targets.RestorePowerWarning([second.Hub with { RestorePowerWarning = null }, unrelated, reconnected]));
        Assert.Equal("Bench (aaa)\nFirst warning.\n\nccc\nSecond warning.",
            targets.RestorePowerWarning([warned, second.Hub with { RestorePowerWarning = null }, other, unrelated]));
    }

    [Fact]
    public void DeviceCommandsRequireEverySelectedPort()
    {
        var a = DeviceOn("aaa", 1);
        var b = a with
        {
            Port = a.Port with { Number = 2, Dfu = true, Reboot = false },
            Needs = new("driver", "Driver", null, "Install the driver."),
        };
        Assert.Null(DeviceActionTargets.For([a, b], DeviceAction.Dfu));
        Assert.Null(DeviceActionTargets.For([a, b], DeviceAction.Restart));
        var signal = DeviceActionTargets.For([a with { Port = a.Port with { Dfu = true } }, b], DeviceAction.Dfu)!;
        Assert.Equal([1, 2], signal.Hubs.Single().Targets.Select(t => t.Port));
        Assert.Null(DeviceActionTargets.For([a, b], DeviceAction.Restore));
        Assert.Null(DeviceActionTargets.For([a, b], DeviceAction.Revive));
        Assert.Null(DeviceActionTargets.For([], DeviceAction.Restore));
        Assert.Null(DeviceActionTargets.For([a with { Hub = a.Hub with { Session = null } }], DeviceAction.Restart));
    }

    [Fact]
    public void SetupAssistantActionsRequireTheWholeSelectionToBeIdleAndIdentified()
    {
        var device = DeviceOn("aaa", 2, "1234");
        var a = device with { Port = device.Port with { Mode = AppleMode.Normal, Request = null, Job = null, Run = null } };
        var b = a with { Hub = a.Hub with { Serial = "bbb", Session = 8 }, Port = a.Port with { Ecid = "5678" } };
        var c = a with { Port = a.Port with { Number = 3, Ecid = "9876" } };
        var targets = DeviceActionTargets.ForSetup([a, b, c])!;
        var hubs = targets.Hubs;
        Assert.Equal([a, c, b], targets.Devices);
        Assert.Equal(["aaa", "bbb"], hubs.Select(hub => hub.Hub));
        Assert.Equal([3u, 8u], hubs.Select(hub => hub.Session));
        Assert.Equal([new Target(2, 7, "1234"), new Target(3, 7, "9876"), new Target(2, 7, "5678")], hubs.SelectMany(hub => hub.Targets));
        Assert.Null(DeviceActionTargets.ForSetup([]));
        Assert.Null(DeviceActionTargets.ForSetup([a, device]));
        Assert.Null(DeviceActionTargets.ForSetup([a, b with { Port = b.Port with { Ecid = null } }]));
        var enrolling = b with { Port = b.Port with { Run = new(b.Port.Ecid, [new(StepKind.RequestEnrollment, StepState.Active)]), Awaiting = null } };
        Assert.Equal("Request enrollment · In progress", enrolling.Status);
        Assert.Null(DeviceActionTargets.ForSetup([a, enrolling]));
        Assert.Null(DeviceActionTargets.ForSetup([a, b with { Hub = b.Hub with { Session = null } }]));
    }

    [Fact]
    public void OneTimeSetupStartsWithSavedChoicesOrTheDesktopLanguage()
    {
        var desktop = new SetupLanguage("en-AU", "en_AU");
        var french = new SetupLanguage("fr-CA", "fr_CA");
        IReadOnlyList<Selection> hubs = [new("a", 3, [new(1, 4, "1234")])];
        Assert.Equal(new AdvanceSetupRequest(hubs, desktop, false, true), DeviceActionTargets.DefaultSetup(hubs, new(false, null, false), desktop));
        Assert.Equal(new AdvanceSetupRequest(hubs, null, true, true), DeviceActionTargets.DefaultSetup(hubs, new(false, null, true), desktop));
        Assert.Equal(new AdvanceSetupRequest(hubs, french, false, true), DeviceActionTargets.DefaultSetup(hubs, new(false, french, false), desktop));
        Assert.Equal(new AdvanceSetupRequest(hubs, french, true, true), DeviceActionTargets.DefaultSetup(hubs, new(false, french, true), desktop));
    }

    [Fact]
    public void RestoreAndSetupKeepTheirOwnOutcomesAndRequestedValues()
    {
        var restore = new OperationStep(StepKind.Restore, StepState.Active, new("iPadOS", "27.2", "24C101"),
            Progress: new("Sending image", 45, ["Prepare", "Install", "Verify"], 1));
        var restoring = RunStepRow.From(restore);
        Assert.Equal("iPadOS 27.2 (24C101)", restoring.Detail);
        Assert.True(restoring.HasProgress);
        Assert.Equal("45%", restoring.Status);
        Assert.Equal("Sending image", restoring.Activity);
        var run = new OperationRun("1234", [
            restore with { State = StepState.Complete },
            new(StepKind.SetRegion, StepState.Complete, Language: new("en-AU", "en_AU")),
            new(StepKind.RequestEnrollment, StepState.Failed, Error: "Sign in on the device."),
        ]);
        Assert.True(run.Finished());
        Assert.False(run.Succeeded());
        Assert.Equal("Request enrollment · Failed", Device.RunStatus(run));
        var rows = run.Steps.Select(RunStepRow.From).ToArray();
        Assert.Equal(["Restore", "Set language and region", "Request enrollment"], rows.Select(r => r.Title));
        Assert.Equal("en-AU · en_AU", rows[1].Detail);
        Assert.Equal("Sign in on the device.", rows[2].Error);
        Assert.All(rows, row =>
        {
            Assert.False(row.HasProgress);
            Assert.False(row.HasActivity);
        });
    }

    [Fact]
    public void RestoreModeHasAnOperatorFacingLabel()
    {
        var device = DeviceOn("aaa", 1);
        var restored = device with
        {
            Port = device.Port with { Mode = AppleMode.Restored, Run = null, Job = null, Request = null, Awaiting = null }
        };
        Assert.Equal("Restore mode", restored.Mode);
        Assert.Equal("Restore mode", restored.Status);
    }

    [Fact]
    public void AReservedPortKeepsItsRunVisibleWhileTheDeviceReconnects()
    {
        var device = DeviceOn("aaa", 1, "1234");
        var run = new OperationRun("1234", [new(StepKind.Restore, StepState.Active)]);
        var before = device with { Port = device.Port with { Run = run } };
        var gap = before with { Port = before.Port with { Generation = 8, Ecid = null, Occupied = false, Device = null } };
        Assert.False(before.SameAttachment(gap));
        Assert.True(before.FollowsRun(gap));
        Assert.Equal("Reconnecting…", gap.Name);
        Assert.Equal(run, before.ActiveRunIn([gap.Hub with { Ports = [gap.Port] }]));
        Assert.Null(before.ActiveRunIn([gap.Hub with { Session = 9, Ports = [gap.Port] }]));
        Assert.Null(before.ActiveRunIn([gap.Hub with { Ports = [gap.Port with { Run = run with { Ecid = "5678" } }] }]));
        Assert.False(before.FollowsRun(gap with { Port = gap.Port with { Ecid = "5678" } }));
        Assert.False(before.FollowsRun(gap with { Port = gap.Port with { Run = null } }));
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
