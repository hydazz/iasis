using System.Text.Json;
using System.Text.Json.Serialization;

namespace Iasis.Agent;

// The agent's messages, field for field as `crates/api` defines them. The
// tests read and write back every example in `crates/api/fixtures`.

/// Everything the agent knows, for a client to show.
public sealed record AgentState
{
    public required string Version { get; init; }
    public required IReadOnlyList<Hub> Hubs { get; init; }
    /// A submitting connection's results in target order, each null until it has one.
    public IReadOnlyList<Ending?>? Submitted { get; init; }
    public required ImageStore Images { get; init; }
    public required Sharing Sharing { get; init; }
    public required Supervision Supervision { get; init; }
    /// What the host lacks that some of Iasis needs.
    public required IReadOnlyList<Prerequisite> Missing { get; init; }
}

public sealed record Hub
{
    public required string Serial { get; init; }
    /// Empty for an unnamed hub.
    public required string Name { get; init; }
    public uint? Session { get; init; }
    /// The IasisOS version the hub runs; empty while it cannot be read.
    public required string Firmware { get; init; }
    /// The IasisOS the agent carries, when the hub runs another.
    public FirmwareUpdate? Update { get; init; }
    public int? UpstreamMbps { get; init; }
    public required Supply Supply { get; init; }
    public Pool? Pool { get; init; }
    public required bool Host { get; init; }
    public required bool Restores { get; init; }
    public required int Brightness { get; init; }
    public required IReadOnlyList<Port> Ports { get; init; }
    public string? Notice { get; init; }
}

public sealed record FirmwareUpdate(string Version, bool Required);

public sealed record Pool(uint BudgetMw, uint AllocatedMw);

public sealed record Port
{
    public required int Number { get; init; }
    public required uint Generation { get; init; }
    public required bool Occupied { get; init; }
    public string? Device { get; init; }
    public required bool Identifying { get; init; }
    public string? Ecid { get; init; }
    public Usb? Usb { get; init; }
    public AppleMode? Mode { get; init; }
    public required Light Light { get; init; }
    public SignalProgress? Request { get; init; }
    public required bool WaitingForPower { get; init; }
    public required bool Powered { get; init; }
    public required bool WrongDfuPort { get; init; }
    public UsbLink? Link { get; init; }
    public PortPower? Power { get; init; }
    public required uint AllocatedMw { get; init; }
    public required bool Short { get; init; }
    public string? Fault { get; init; }
    public required bool Dfu { get; init; }
    public required bool Reboot { get; init; }
    /// Restore, or Revive.
    public required bool Restore { get; init; }
    public required bool Enabled { get; init; }
    public Job? Job { get; init; }
    public string? Awaiting { get; init; }
    public Progress? Stage { get; init; }
    public Ending? Result { get; init; }
    /// The id of the missing prerequisite the target's restore waits for.
    public string? Needs { get; init; }
}

public sealed record Light(Hue Hue, bool Pulse, bool Dim);

public sealed record PortPower(PowerSource Source, int Millivolts, int Milliamps);

public sealed record Job(Operation Operation, RestorePhase Phase);

public sealed record Progress(string Name, int Percent, IReadOnlyList<string> Steps, int Step);

public sealed record Ending(
    Operation Operation,
    string? Ecid,
    RestorePhase Phase,
    string Message);

public sealed record ImageStore(IReadOnlyList<Image> Images, Fetch? Fetch);

public sealed record Image(
    string Id,
    string Os,
    string Version,
    string Build,
    long Bytes,
    IReadOnlyList<string> Models,
    bool Ready,
    string? Error,
    string? Path,
    bool Fetched,
    bool InUse);

public sealed record Fetch(string Id, string Version, string Build, long Bytes, long Done);

public sealed record Sharing(bool Enabled, string? Problem, IReadOnlyList<SharingPort> Ports);

public sealed record SharingPort(string Hub, int Port, bool Active, string? Problem);

/// The identity the host pairs with as supervisor: its certificate's common
/// name and the certificate in base64 DER.
public sealed record Supervision(string? Name, string? Certificate, string? Problem);

/// Software the host lacks, and what goes without it.
public sealed record Prerequisite(string Id, string Name, string Url, string Reason);

/// The targets selected on one hub, in the session the client saw.
public sealed record Selection(string Hub, uint Session, IReadOnlyList<Target> Targets);

/// One attachment, and the device on it when it is identified.
public sealed record Target(int Port, uint Generation, string? Ecid);

public sealed record Planned(string Hub, int Port, Offer? Offer);

public sealed record Offer(Release? Latest, Release? Held, string? Problem);

public sealed record Release(string Os, string Version, string Build, long Bytes);

public sealed record CatalogModel(string Identifier, string Name);

[JsonConverter(typeof(SnakeCaseEnum<Supply>))]
public enum Supply { Brick, Host, Off }

[JsonConverter(typeof(SnakeCaseEnum<Usb>))]
public enum Usb { Absent, Other, Ambiguous, Apple }

[JsonConverter(typeof(SnakeCaseEnum<AppleMode>))]
public enum AppleMode { Dfu, PortDfu, Recovery, Kis, KisDfu, Restored, Normal }

[JsonConverter(typeof(SnakeCaseEnum<Hue>))]
public enum Hue { Off, Red, Green, White, Blue, Amber, Purple }

[JsonConverter(typeof(SnakeCaseEnum<SignalProgress>))]
public enum SignalProgress { EnteringDfu, DfuUnconfirmed, Restarting }

[JsonConverter(typeof(SnakeCaseEnum<UsbLink>))]
public enum UsbLink { Usb, Usb2, Usb3 }

[JsonConverter(typeof(SnakeCaseEnum<PowerSource>))]
public enum PowerSource { Pd, Usb }

[JsonConverter(typeof(SnakeCaseEnum<Operation>))]
public enum Operation { Restore, Revive }

/// The hub's restore phases keep their own names on the wire.
[JsonConverter(typeof(JsonStringEnumConverter<RestorePhase>))]
public enum RestorePhase { Preparing, Running, Reported, Unconfirmed, Failed }

public sealed class SnakeCaseEnum<T>() : JsonStringEnumConverter<T>(JsonNamingPolicy.SnakeCaseLower)
    where T : struct, Enum;

/// A request, tagged with its `op`.
[JsonPolymorphic(TypeDiscriminatorPropertyName = "op")]
[JsonDerivedType(typeof(StateRequest), "state")]
[JsonDerivedType(typeof(WatchRequest), "watch")]
[JsonDerivedType(typeof(PlanRequest), "plan")]
[JsonDerivedType(typeof(RestoreRequest), "restore")]
[JsonDerivedType(typeof(ReviveRequest), "revive")]
[JsonDerivedType(typeof(DfuRequest), "dfu")]
[JsonDerivedType(typeof(RebootRequest), "reboot")]
[JsonDerivedType(typeof(PowerRequest), "power")]
[JsonDerivedType(typeof(DebugUsbRequest), "debug_usb")]
[JsonDerivedType(typeof(RenameRequest), "rename")]
[JsonDerivedType(typeof(BrightnessRequest), "brightness")]
[JsonDerivedType(typeof(HostBudgetRequest), "host_budget")]
[JsonDerivedType(typeof(UpdateRequest), "update")]
[JsonDerivedType(typeof(SharingRequest), "sharing")]
[JsonDerivedType(typeof(ImportSupervisionRequest), "import_supervision")]
[JsonDerivedType(typeof(ExportSupervisionRequest), "export_supervision")]
[JsonDerivedType(typeof(DownloadRequest), "download")]
[JsonDerivedType(typeof(ReleasesRequest), "releases")]
[JsonDerivedType(typeof(ImportRequest), "import")]
[JsonDerivedType(typeof(RemoveRequest), "remove")]
[JsonDerivedType(typeof(CatalogRequest), "catalog")]
public abstract record Request
{
    /// The bytes that follow the request on the connection.
    public virtual long Payload() => 0;
}

public sealed record StateRequest : Request;
public sealed record WatchRequest : Request;
public sealed record PlanRequest(IReadOnlyList<Selection> Hubs) : Request;
public sealed record RestoreRequest(IReadOnlyList<Selection> Hubs, bool Latest) : Request;
public sealed record ReviveRequest(IReadOnlyList<Selection> Hubs, bool Latest) : Request;
public sealed record DfuRequest(string Hub, int Port) : Request;
public sealed record RebootRequest(string Hub, int Port) : Request;
public sealed record PowerRequest(string Hub, int Port, bool On) : Request;
public sealed record DebugUsbRequest(string Hub, int Port, bool On) : Request;
public sealed record RenameRequest(string Hub, string Name) : Request;
public sealed record BrightnessRequest(string Hub, int Percent) : Request;
public sealed record HostBudgetRequest(string Hub, int Milliamps) : Request;

public sealed record UpdateRequest(string Hub, long? Bytes) : Request
{
    public override long Payload() => Bytes ?? 0;
}

public sealed record SharingRequest(bool Enabled) : Request;
/// Replace the supervision identity with the one in a PKCS #12 archive, in base64.
public sealed record ImportSupervisionRequest(string Pkcs12, string Password) : Request;
/// The supervision identity as a PKCS #12 archive protected by the password.
public sealed record ExportSupervisionRequest(string Password) : Request;
public sealed record DownloadRequest(string Model, string? Build) : Request;
public sealed record ReleasesRequest(string Model) : Request;

public sealed record ImportRequest(string Name, long Bytes) : Request
{
    public override long Payload() => Bytes;
}

public sealed record RemoveRequest(string Image) : Request;
public sealed record CatalogRequest : Request;

/// A reply: `"ok"`, or an object whose one member names what it carries.
[JsonConverter(typeof(ReplyConverter))]
public abstract record Reply;

public sealed record OkReply : Reply;
public sealed record ErrorReply(string Message) : Reply;
public sealed record StateReply(AgentState State) : Reply;
public sealed record CatalogReply(IReadOnlyList<CatalogModel> Models) : Reply;
public sealed record ReleasesReply(IReadOnlyList<Release> Releases) : Reply;
public sealed record PlanReply(IReadOnlyList<Planned> Planned) : Reply;
/// The supervision identity as a PKCS #12 archive in base64.
public sealed record SupervisionReply(string Pkcs12) : Reply;

public sealed class ReplyConverter : JsonConverter<Reply>
{
    public override Reply Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        if (reader.TokenType == JsonTokenType.String)
        {
            return reader.GetString() == "ok" ? new OkReply() : throw new JsonException("unknown reply");
        }
        if (reader.TokenType != JsonTokenType.StartObject || !reader.Read())
        {
            throw new JsonException("a reply is a string or an object");
        }
        var kind = reader.GetString();
        reader.Read();
        Reply reply = kind switch
        {
            "error" => new ErrorReply(reader.GetString() ?? ""),
            "state" => new StateReply(Read(ref reader, AgentJson.Default.AgentState)),
            "catalog" => new CatalogReply(Read(ref reader, AgentJson.Default.IReadOnlyListCatalogModel)),
            "releases" => new ReleasesReply(Read(ref reader, AgentJson.Default.IReadOnlyListRelease)),
            "plan" => new PlanReply(Read(ref reader, AgentJson.Default.IReadOnlyListPlanned)),
            "supervision" => new SupervisionReply(reader.GetString() ?? ""),
            _ => throw new JsonException($"unknown reply {kind}"),
        };
        reader.Read();
        return reply;
    }

    private static T Read<T>(ref Utf8JsonReader reader, System.Text.Json.Serialization.Metadata.JsonTypeInfo<T> info) =>
        JsonSerializer.Deserialize(ref reader, info) ?? throw new JsonException("an empty reply");

    public override void Write(Utf8JsonWriter writer, Reply value, JsonSerializerOptions options)
    {
        if (value is OkReply)
        {
            writer.WriteStringValue("ok");
            return;
        }
        writer.WriteStartObject();
        switch (value)
        {
            case ErrorReply error:
                writer.WriteString("error", error.Message);
                break;
            case StateReply state:
                writer.WritePropertyName("state");
                JsonSerializer.Serialize(writer, state.State, AgentJson.Default.AgentState);
                break;
            case CatalogReply catalog:
                writer.WritePropertyName("catalog");
                JsonSerializer.Serialize(writer, catalog.Models, AgentJson.Default.IReadOnlyListCatalogModel);
                break;
            case ReleasesReply releases:
                writer.WritePropertyName("releases");
                JsonSerializer.Serialize(writer, releases.Releases, AgentJson.Default.IReadOnlyListRelease);
                break;
            case PlanReply plan:
                writer.WritePropertyName("plan");
                JsonSerializer.Serialize(writer, plan.Planned, AgentJson.Default.IReadOnlyListPlanned);
                break;
            case SupervisionReply supervision:
                writer.WriteString("supervision", supervision.Pkcs12);
                break;
        }
        writer.WriteEndObject();
    }
}

[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.SnakeCaseLower,
    DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    AllowOutOfOrderMetadataProperties = true)]
[JsonSerializable(typeof(AgentState))]
[JsonSerializable(typeof(Request))]
[JsonSerializable(typeof(Reply))]
[JsonSerializable(typeof(IReadOnlyList<Request>))]
[JsonSerializable(typeof(IReadOnlyList<Reply>))]
[JsonSerializable(typeof(IReadOnlyList<CatalogModel>))]
[JsonSerializable(typeof(IReadOnlyList<Release>))]
[JsonSerializable(typeof(IReadOnlyList<Planned>))]
public sealed partial class AgentJson : JsonSerializerContext;
