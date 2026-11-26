using System.Collections.ObjectModel;
using System.ComponentModel;
using Iasis.Agent;

namespace Iasis;

public abstract class Row : INotifyPropertyChanged
{
    public event PropertyChangedEventHandler? PropertyChanged;
    protected void Refresh() => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
}

[Microsoft.UI.Xaml.Data.Bindable]
public sealed class HubRow(Hub hub) : Row
{
    internal Hub Hub { get; private set; } = hub;
    public string Title => string.IsNullOrWhiteSpace(Hub.Name) ? Hub.Serial : $"{Hub.Name} ({Hub.Serial})";
    public ObservableCollection<PortRow> Ports { get; } = [];

    public override string ToString() => Title;

    public void Update(Hub value)
    {
        var rename = Hub.Name != value.Name;
        Hub = value;
        if (rename) Refresh();
    }
}

public sealed class PortRow(Device device) : Row
{
    public Device Device { get; private set; } = device;
    public string Title => $"Port {Device.Port.Number}  ·  {Device.Name}";
    public string Status => Device.Status;

    public override string ToString() => Status.Length == 0 ? Title : $"{Title}, {Status}";

    public void Update(Device value)
    {
        var previous = (Title, Status);
        Device = value;
        if (previous != (Title, Status)) Refresh();
    }
}

public sealed class ImageRow(Agent.Image image) : Row
{
    public Agent.Image Image { get; private set; } = image;
    public string Title => $"{Image.Os} {Image.Version} ({Image.Build})";
    public string Detail => $"{Image.Id}  ·  {DeviceActionTargets.Size(Image.Bytes)}\n{string.Join(", ", Image.Models)}";
    public string Status => Image.Error ?? (Image.InUse ? "In use" : Image.Ready ? "Downloaded" : "Waiting for download");

    public override string ToString() => $"{Title}, {Status}";

    public void Update(Agent.Image value)
    {
        var previous = (Title, Detail, Status);
        Image = value;
        if (previous != (Title, Detail, Status)) Refresh();
    }
}
