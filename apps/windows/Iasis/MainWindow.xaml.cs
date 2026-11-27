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
                    if (!row.Device.SameAttachment(snapshot) && !row.Device.FollowsRun(snapshot)) Ports.SelectedItems.Remove(row);
                    row.Update(snapshot);
                }
            }
        }
        Empty.Visibility = ShowWhen(next is not null && hubs.Count == 0);
        if (HubList.SelectedItem is null && hubs.Count > 0) HubList.SelectedIndex = 0;
        ShowHub();
        ShowImages();
        ShowIdentity();
        ShowPostRestore();
        if (!(next?.Missing ?? []).SequenceEqual(old?.Missing ?? [])) ShowPrerequisites();
        ShowSelection();
        StateChanged?.Invoke();
    }

    private void SelectionChanged(object sender, SelectionChangedEventArgs args)
    {
        HideDeviceMenu();
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
        if ((port.Ecid ?? port.Run?.Ecid) is { } ecid) details.Add($"ECID {ecid}");
        if (d.Mode.Length > 0) details.Add(d.Mode);
        if (port.Power is { } power)
        {
            var label = power.Source == PowerSource.Pd ? "PD contract" : "USB allowance";
            details.Add($"{label} · {power.Millivolts / 1000.0:g} V · {power.Milliamps} mA");
        }
        if (port.Short) details.Add("The device reports insufficient power.");
        if (d.Status.Length > 0 && d.Status != d.Mode && d.Status != d.Needs?.Reason &&
            (port.Run is null || d.Status != Device.RunStatus(port.Run))) details.Add(d.Status);
        Details.Text = string.Join("\n", details);
        ShowRun(Steps, port.Run);
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
        ShowPostRestore();
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

    private void EnterDfu(object sender, RoutedEventArgs args) => Act(DeviceAction.Dfu);
    private void RestartDevice(object sender, RoutedEventArgs args) => Act(DeviceAction.Restart);
    private void ReviveDevice(object sender, RoutedEventArgs args) => Act(DeviceAction.Revive);
    private void RestoreDevice(object sender, RoutedEventArgs args) => Act(DeviceAction.Restore);

    internal void Act(DeviceAction action, Device? trayDevice = null)
    {
        var selected = trayDevice is null ? Selected : [trayDevice];
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
            var current = trayDevice is null ? Selected : Devices.ToArray();
            if ((trayDevice is null && current.Length != selected.Length) ||
                selected.Any(old => !current.Any(d => old.SameAttachment(d) && d.Supports(action))))
                throw new InvalidOperationException("The devices available for this action changed. Select them again.");
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
            Title = request switch
            {
                RestoreRequest => "Restore",
                ReviveRequest => "Revive",
                AdvanceSetupRequest => "Advance Setup",
                _ => throw new ArgumentOutOfRangeException(nameof(request)),
            },
            Message = "Starting…",
            IsOpen = true,
            IsClosable = false,
        };
        var devices = new StackPanel { Spacing = 8 };
        var steps = targets.Devices.Select(device =>
        {
            devices.Children.Add(new TextBlock { Text = $"{device.Place} · {device.Name}", TextWrapping = TextWrapping.Wrap });
            var list = new ItemsControl { ItemTemplate = (DataTemplate)Root.Resources["OperationStepTemplate"], Margin = new Thickness(12, 0, 0, 0) };
            devices.Children.Add(list);
            return list;
        }).ToArray();
        bar.Content = devices;
        Submissions.Children.Add(bar);
        bar.Closed += (_, _) => Submissions.Children.Remove(bar);
        try
        {
            await foreach (var update in AgentClient.Local.Watch(stop.Token, request))
            {
                if (update.Submitted is not { } results || results.Count != targets.Devices.Count)
                    throw new InvalidDataException("Results are missing for some selected devices.");
                var runs = results.Select((run, i) => run ?? targets.Devices[i].ActiveRunIn(update.Hubs)).ToArray();
                bar.Message = runs.Any(run => run is null) ? "Waiting for device…" : "";
                for (var i = 0; i < runs.Length; i++)
                    ShowRun(steps[i], runs[i]);
                if (results.All(run => run?.Finished() == true))
                {
                    var failed = results.Any(run => run!.Steps.Any(step => step.State == StepState.Failed) &&
                        !run.Steps.Any(step => step.State == StepState.Complete));
                    bar.Severity = failed ? InfoBarSeverity.Error :
                        results.All(run => run!.Succeeded()) ? InfoBarSeverity.Success : InfoBarSeverity.Warning;
                    return;
                }
            }
            throw new IOException("The connection closed before all devices reported their results.");
        }
        catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
        catch (Exception error)
        {
            bar.Severity = InfoBarSeverity.Error;
            bar.Message = $"{bar.Message}\n{error.Message}";
        }
        finally { bar.IsClosable = true; }
    }

    private static void ShowRun(ItemsControl list, OperationRun? run)
    {
        var rows = run?.Steps.Select(RunStepRow.From).ToArray() ?? [];
        list.Visibility = ShowWhen(rows.Length > 0);
        if (list.ItemsSource is not RunStepRow[] displayed || !displayed.SequenceEqual(rows))
            list.ItemsSource = rows;
    }
}
