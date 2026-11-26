using System.Globalization;

namespace Iasis.Agent;

/// A snapshot of the attachment shown in the window or notification menu.
public sealed record Device(Hub Hub, Port Port, Prerequisite? Needs)
{
    public string Place => $"{HubTitle(Hub)}, port {Port.Number}";
    public string Name => !Port.Enabled ? "Off" : !Port.Occupied ? "Empty" :
        Port.Device ?? (Port.Identifying ? "Identifying…" : "Unknown device");

    public static string HubTitle(Hub hub) => string.IsNullOrWhiteSpace(hub.Name) ? hub.Serial : hub.Name;

    public string Status => Port switch
    {
        { Fault: { } fault } => fault,
        { Awaiting: not null } => "Waiting for image",
        { Job: { } job, Stage: { } stage } => $"{job.Operation} · {stage.Name} · {stage.Percent}%",
        { Job: { } job } => Phase(job.Operation, job.Phase),
        { Request: SignalProgress.EnteringDfu } => "Entering DFU",
        { Request: SignalProgress.DfuUnconfirmed } => "DFU not confirmed",
        { Request: SignalProgress.Restarting } => "Restarting",
        { Result: { } result } => $"{Phase(result.Operation, result.Phase)} · {result.Message}",
        { WaitingForPower: true } => "Waiting for power",
        { WrongDfuPort: true } => "Connect the cable to the Mac’s DFU port.",
        _ when Needs is { } missing => missing.Reason,
        _ => Mode,
    };

    public string Mode => Port.Mode switch
    {
        AppleMode.Dfu => "DFU",
        AppleMode.PortDfu => "Port DFU",
        AppleMode.KisDfu => "DFU over Debug USB",
        AppleMode.Recovery => "Recovery",
        AppleMode.Kis => "Debug USB",
        AppleMode.Restored => "Restore service",
        AppleMode.Normal => "Started up",
        _ => "",
    };

    public static string Phase(Operation operation, RestorePhase phase) => phase switch
    {
        RestorePhase.Reported => $"{operation} complete",
        RestorePhase.Unconfirmed => $"{operation} outcome unknown",
        RestorePhase.Failed => $"{operation} failed",
        RestorePhase.Preparing => $"Preparing to {operation.ToString().ToLowerInvariant()}",
        _ => operation == Operation.Restore ? "Restoring" : "Reviving",
    };

    public bool SameAttachment(Device other) =>
        Hub.Serial == other.Hub.Serial && Hub.Session == other.Hub.Session &&
        Port.Number == other.Port.Number && Port.Generation == other.Port.Generation &&
        (Port.Ecid is null || Port.Ecid == other.Port.Ecid);
}

public enum DeviceAction { Dfu, Restart, Revive, Restore }

public sealed record DeviceActionTargets(IReadOnlyList<Device> Devices, IReadOnlyList<Selection> Hubs)
{
    public static DeviceActionTargets? For(IEnumerable<Device> devices, DeviceAction action)
    {
        var selected = devices.ToArray();
        if (action is DeviceAction.Restore or DeviceAction.Revive &&
            selected.Any(d => !d.Port.Occupied || !d.Port.Restore || d.Needs is not null))
        {
            return null;
        }
        var targets = selected.Where(d => action switch
        {
            DeviceAction.Dfu => d.Port.Dfu,
            DeviceAction.Restart => d.Port.Reboot,
            _ => true,
        }).GroupBy(d => d.Hub.Serial).ToArray();

        if (targets.Length == 0 || targets.Any(group => group.First().Hub.Session is null))
        {
            return null;
        }

        // The watch reports submitted results in hub order, then target order.
        var hubs = targets.Select(group => new Selection(group.Key, group.First().Hub.Session!.Value,
            group.Select(d => new Target(d.Port.Number, d.Port.Generation, d.Port.Ecid)).ToArray())).ToArray();
        return new(targets.SelectMany(group => group).ToArray(), hubs);
    }

    public Request Request(DeviceAction action, bool latest = false) => action switch
    {
        DeviceAction.Dfu => new DfuRequest(Hubs),
        DeviceAction.Restart => new RebootRequest(Hubs),
        DeviceAction.Revive => new ReviveRequest(Hubs, latest),
        DeviceAction.Restore => new RestoreRequest(Hubs, latest),
        _ => throw new ArgumentOutOfRangeException(nameof(action)),
    };

    public ErasePlan Review(IReadOnlyList<Planned> plan)
    {
        var lines = new List<string>();
        var blocked = false;
        var newer = false;
        foreach (var device in Devices)
        {
            var entry = plan.SingleOrDefault(p => p.Hub == device.Hub.Serial && p.Port == device.Port.Number)
                ?? throw new InvalidDataException($"The service returned no plan for {device.Place}.");
            var label = $"{device.Place} ({device.Hub.Serial}): {device.Name}";
            if (device.Port.Ecid is { } ecid)
            {
                label += $" · ECID {ecid}";
            }
            var offer = entry.Offer;
            var image = offer switch
            {
                null => "Image will be selected when the device is identified",
                { Held: { } held, Latest: { } latest } when IsNewer(latest, held) =>
                    $"{ReleaseName(held)} downloaded, {ReleaseName(latest)} available",
                { Held: { } held } => $"{ReleaseName(held)} downloaded",
                { Latest: { } latest } => $"Download {ReleaseName(latest)} ({Size(latest.Bytes)})",
                _ => offer.Problem ?? "No signed image is available",
            };
            newer |= offer is { Held: { } local, Latest: { } remote } && IsNewer(remote, local);
            blocked |= offer is { Held: null, Latest: null };
            lines.Add($"{label}\n{image}");
        }
        return new(string.Join("\n\n", lines), blocked, newer);
    }

    private static bool IsNewer(Release latest, Release held)
    {
        var compare = CultureInfo.InvariantCulture.CompareInfo;
        var version = compare.Compare(latest.Version, held.Version, CompareOptions.NumericOrdering);
        return version > 0 || (version == 0 &&
            compare.Compare(latest.Build, held.Build, CompareOptions.NumericOrdering) > 0);
    }

    public static string ReleaseName(Release release) => $"{release.Os} {release.Version} ({release.Build})";
    public static string Size(long bytes) => bytes >= 1_000_000_000 ? $"{bytes / 1e9:0.#} GB" : $"{bytes / 1e6:0.#} MB";
}

public sealed record ErasePlan(string Summary, bool Blocked, bool NewerAvailable);
