using System.Collections.ObjectModel;
using Iasis.Agent;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;
using Microsoft.Win32;

namespace Iasis;

public sealed partial class MainWindow : Window
{
    private readonly CancellationTokenSource stop = new();
    private readonly ObservableCollection<HubRow> hubs = [];
    private readonly ObservableCollection<ImageRow> images = [];
    private AgentState? state;
    private bool busy;
    private bool initialized;
    private bool quitting;

    internal event Action? StateChanged;
    internal AgentState? State => state;
    internal IEnumerable<Device> Devices => hubs.SelectMany(hub => hub.Ports).Select(row => row.Device);
    private Device[] Selected => Ports.SelectedItems.Cast<PortRow>().Select(row => row.Device).ToArray();

    public MainWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        SystemBackdrop = new MicaBackdrop();
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "Iasis.ico"));
        AppWindow.Closing += (_, closing) =>
        {
            if (!quitting)
            {
                closing.Cancel = true;
                AppWindow.Hide();
            }
        };
        ((CollectionViewSource)Root.Resources["Groups"]).Source = hubs;
        HubList.ItemsSource = hubs;
        ImageList.ItemsSource = images;
        using var loginSettings = Registry.CurrentUser.OpenSubKey(LoginPath);
        Login.IsOn = loginSettings?.GetValue("Iasis") is not null;
        initialized = true;
        Navigation.SelectedItem = DevicesTab;
        Show(null);
        _ = Follow();
    }

    public void Quit()
    {
        quitting = true;
        stop.Cancel();
        Close();
    }

    internal void Open(bool settings = false)
    {
        AppWindow.Show();
        Activate();
        Navigation.SelectedItem = settings ? SettingsTab : DevicesTab;
    }

    private void Navigate(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (!initialized) return;
        var item = sender.SelectedItem as NavigationViewItem;
        DevicesPage.Visibility = ShowWhen(item == DevicesTab);
        ImagesPage.Visibility = ShowWhen(item == ImagesTab);
        SettingsPage.Visibility = ShowWhen(item == SettingsTab);
    }

    private static Visibility ShowWhen(bool show) => show ? Visibility.Visible : Visibility.Collapsed;

    private async Task Follow()
    {
        while (!stop.IsCancellationRequested)
        {
            string reason;
            try
            {
                await foreach (var next in AgentClient.Local.Watch(stop.Token)) Show(next);
                reason = "The Iasis service disconnected.";
            }
            catch (OperationCanceledException) when (stop.IsCancellationRequested) { return; }
            catch (AgentRefusedException error) { reason = error.Message; }
            catch (System.Text.Json.JsonException) { reason = "The app and the Iasis service use different versions of the agent protocol."; }
            catch (Exception error) { reason = $"Could not connect to the Iasis service. {error.Message}"; }
            if (stop.IsCancellationRequested) return;
            Show(null);
            Connection.Message = reason;
            try { await Task.Delay(TimeSpan.FromSeconds(1), stop.Token); }
            catch (OperationCanceledException) { return; }
        }
    }

    private void Show(AgentState? next)
    {
        var old = state;
        state = next;
        Connection.IsOpen = next is null;
        if (next is null) Connection.Message = "Connecting to the Iasis service…";
        foreach (var removed in hubs.Where(h => next?.Hubs.Any(value => value.Serial == h.Hub.Serial) != true).ToArray())
        {
            hubs.Remove(removed);
        }
        foreach (var hub in next?.Hubs ?? [])
        {
            var group = hubs.FirstOrDefault(h => h.Hub.Serial == hub.Serial);
            if (group is null) hubs.Add(group = new HubRow(hub));
            else group.Update(hub);
            foreach (var removed in group.Ports.Where(row => hub.Ports.All(port => port.Number != row.Device.Port.Number)).ToArray())
            {
                group.Ports.Remove(removed);
            }
            foreach (var port in hub.Ports)
            {
                var snapshot = new Device(hub, port, next!.Missing.FirstOrDefault(m => m.Id == port.Needs));
                var row = group.Ports.FirstOrDefault(existing => existing.Device.Port.Number == port.Number);
                if (row is null) group.Ports.Add(new PortRow(snapshot));
                else
                {
                    if (!row.Device.SameAttachment(snapshot)) Ports.SelectedItems.Remove(row);
                    row.Update(snapshot);
                }
            }
        }
        Empty.Visibility = ShowWhen(next is not null && hubs.Count == 0);
        if (HubList.SelectedItem is null && hubs.Count > 0) HubList.SelectedIndex = 0;
        ShowHub();
        ShowImages();
        ShowIdentity();
        if (!(next?.Missing ?? []).SequenceEqual(old?.Missing ?? [])) ShowPrerequisites();
        ShowSelection();
        StateChanged?.Invoke();
    }

    private void SelectionChanged(object sender, SelectionChangedEventArgs args)
    {
        if (initialized) ShowSelection();
    }

    private void ShowSelection()
    {
        var selected = Selected;
        Dfu.IsEnabled = Can(DeviceAction.Dfu);
        Restart.IsEnabled = Can(DeviceAction.Restart);
        Restore.IsEnabled = Revive.IsEnabled = Can(DeviceAction.Restore);
        var reason = selected.Select(d => d.Needs).FirstOrDefault(p => p is not null)?.Reason;
        ToolTipService.SetToolTip(Revive, reason ?? "Repair system software");
        ToolTipService.SetToolTip(Restore, reason ?? "Erase and reinstall system software");
        Inspector.Visibility = ShowWhen(selected.Length == 1);
        if (selected is not [var d]) return;
        DeviceTitle.Text = d.Name;
        var port = d.Port;
        var details = new List<string> { d.Place };
        if (port.Ecid is { } ecid) details.Add($"ECID {ecid}");
        if (d.Mode.Length > 0) details.Add(d.Mode);
        if (port.Power is { } power) details.Add($"{power.Millivolts / 1000.0:g} V · {power.Milliamps} mA");
        if (d.Status.Length > 0 && d.Status != d.Mode && d.Status != d.Needs?.Reason) details.Add(d.Status);
        Details.Text = string.Join("\n", details);
        Progress.Visibility = ShowWhen(port.Stage is not null);
        Progress.Value = port.Stage?.Percent ?? 0;
        Steps.Text = port.Stage is { } stage ? string.Join("\n", stage.Steps.Select((s, i) =>
            i == stage.Step ? $"{s} (in progress)" : i < stage.Step ? $"{s} (done)" : s)) : "";
        Needs.Text = d.Needs?.Reason ?? "";
        Needs.Visibility = ShowWhen(d.Needs is not null);
        GetPrerequisite.Visibility = ShowWhen(d.Needs?.Url is not null);
        if (d.Needs is { Url: { } url } needed)
        {
            GetPrerequisite.NavigateUri = new Uri(url);
            GetPrerequisite.Content = $"Get {needed.Name}";
        }

        bool Can(DeviceAction action) => !busy && DeviceActionTargets.For(selected, action) is not null;
    }

    private void ShowPrerequisites()
    {
        Prerequisites.Children.Clear();
        foreach (var item in state?.Missing ?? [])
        {
            var bar = new InfoBar
            {
                Title = item.Name,
                Message = item.Reason,
                IsOpen = true,
                IsClosable = false,
                Severity = InfoBarSeverity.Warning,
            };
            if (item.Url is { } url)
            {
                bar.ActionButton = new HyperlinkButton { Content = $"Get {item.Name}", NavigateUri = new Uri(url) };
            }
            Prerequisites.Children.Add(bar);
        }
    }

    private async Task Run(Func<Task> work)
    {
        if (busy || quitting) return;
        busy = true;
        RefreshCommands();
        try { await work(); }
        catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
        catch (Exception error)
        {
            Problem.Message = error.Message;
            Problem.IsOpen = true;
        }
        finally
        {
            busy = false;
            if (!quitting) RefreshCommands();
        }
    }

    private void RefreshCommands()
    {
        ShowSelection();
        ShowHub();
        ShowIdentity();
        ImageButtons();
    }

    private async Task Send(Request request, FileStream? file = null)
    {
        if (await AgentClient.Local.Ask(request, stop.Token, file) is not OkReply)
            throw new InvalidDataException("The Iasis service returned an unexpected reply.");
    }

    private ContentDialog Dialog(string title, object content, string primary = "") => new()
    {
        XamlRoot = Root.XamlRoot,
        Title = title,
        Content = content,
        PrimaryButtonText = primary,
        CloseButtonText = "Cancel",
        DefaultButton = ContentDialogButton.Close,
    };

    private void EnterDfu(object sender, RoutedEventArgs args) => Act(DeviceAction.Dfu, Selected);
    private void RestartDevice(object sender, RoutedEventArgs args) => Act(DeviceAction.Restart, Selected);
    private void ReviveDevice(object sender, RoutedEventArgs args) => Act(DeviceAction.Revive, Selected);
    private void RestoreDevice(object sender, RoutedEventArgs args) => Act(DeviceAction.Restore, Selected);

    internal void Act(DeviceAction action, IReadOnlyList<Device> selected)
    {
        _ = Run(async () =>
        {
            var targets = DeviceActionTargets.For(selected, action);
            if (targets is null) return;
            var latest = false;
            if (action == DeviceAction.Restore)
            {
                if (await AgentClient.Local.Ask(new PlanRequest(targets.Hubs), stop.Token) is not PlanReply reply)
                    throw new InvalidDataException("The service returned no restore plan.");
                var plan = targets.Review(reply.Planned);
                var content = new StackPanel { Spacing = 12 };
                content.Children.Add(new TextBlock { Text = "All data on the following devices will be erased.", TextWrapping = TextWrapping.Wrap });
                content.Children.Add(new TextBlock { Text = plan.Summary, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true });
                var newest = new CheckBox { Content = "Download the latest signed release instead of using the downloaded image" };
                if (plan.NewerAvailable) content.Children.Add(newest);
                var dialog = Dialog(plan.Blocked ? "No restore image available" : "Erase and restore?",
                    new ScrollViewer { Content = content, MaxHeight = 400 }, plan.Blocked ? "" : "Erase and restore");
                if (await dialog.ShowAsync() != ContentDialogResult.Primary || plan.Blocked) return;
                latest = newest.IsChecked == true;
            }
            if (targets.Devices.Any(old => !Devices.Any(old.SameAttachment)))
                throw new InvalidOperationException("The attachment changed. Select the device again.");
            var request = targets.Request(action, latest);
            if (action is DeviceAction.Restore or DeviceAction.Revive)
                _ = Track(request, targets);
            else
                await Send(request);
        });
    }

    private async Task Track(Request request, DeviceActionTargets targets)
    {
        var bar = new InfoBar
        {
            Title = request is ReviveRequest ? "Revive" : "Restore",
            Message = "Submitting…",
            IsOpen = true,
            IsClosable = false,
        };
        Submissions.Children.Add(bar);
        bar.Closed += (_, _) => Submissions.Children.Remove(bar);
        try
        {
            await foreach (var update in AgentClient.Local.Watch(stop.Token, request))
            {
                if (update.Submitted is not { } results || results.Count != targets.Devices.Count)
                    throw new InvalidDataException("The service returned incomplete submission details.");
                bar.Message = string.Join("\n", results.Select((r, i) =>
                    $"{targets.Devices[i].Place} · {targets.Devices[i].Name}: " + (r is null ? "In progress" : $"{Device.Phase(r.Operation, r.Phase)} · {r.Message}")));
                if (results.All(r => r is not null))
                {
                    bar.Severity = results.Any(r => r!.Phase == RestorePhase.Failed) ? InfoBarSeverity.Error :
                        results.Any(r => r!.Phase == RestorePhase.Unconfirmed) ? InfoBarSeverity.Warning : InfoBarSeverity.Success;
                    return;
                }
            }
            throw new IOException("The service disconnected before reporting every outcome.");
        }
        catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
        catch (Exception error)
        {
            bar.Severity = InfoBarSeverity.Error;
            bar.Message = $"{bar.Message}\n{error.Message}";
        }
        finally { bar.IsClosable = true; }
    }
}
