using Iasis.Agent;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;

namespace Iasis;

public sealed partial class MainWindow
{
    private MenuFlyout? deviceMenu;

    private void HideDeviceMenu()
    {
        var menu = deviceMenu;
        deviceMenu = null;
        menu?.Hide();
    }

    private void PortContextCanceled(UIElement sender, RoutedEventArgs args) => HideDeviceMenu();

    private async void PortContextRequested(UIElement sender, ContextRequestedEventArgs args)
    {
        var source = args.OriginalSource as DependencyObject;
        while (source is not null && source != Ports && source is not ListViewItem)
            source = VisualTreeHelper.GetParent(source);
        if (source is not ListViewItem { Content: PortRow row } container || busy || state is null) return;
        args.Handled = true;
        HideDeviceMenu();
        if (!Ports.SelectedItems.Contains(row))
        {
            Ports.SelectedItems.Clear();
            Ports.SelectedItems.Add(row);
        }
        var selected = Selected;
        var targets = DeviceActionTargets.SetupSelection(selected);
        var hasPoint = args.TryGetPosition(container, out var point);
        var menu = new MenuFlyout();
        var shown = false;
        var actions = new List<(MenuFlyoutItem Item, Func<Device[], bool> Available)>();
        deviceMenu = menu;
        AddDeviceAction(DeviceAction.Dfu, Dfu.Label);
        AddDeviceAction(DeviceAction.Restart, Restart.Label);
        AddDeviceAction(DeviceAction.Revive, Revive.Label);
        AddDeviceAction(DeviceAction.Restore, Restore.Label);
        menu.Closed += (_, _) =>
        {
            StateChanged -= Refresh;
            if (deviceMenu == menu) deviceMenu = null;
        };
        Refresh();
        if (targets is not null)
        {
            try
            {
                await Send(new AdvanceSetupRequest(targets, null, Dep: false, CheckOnly: true));
                if (deviceMenu != menu || !IsCurrent(Selected)) return;
                AddAction("Advance Setup…", current => current.All(d => d.CanCheckSetup),
                    () => _ = Run(() => AdvanceSetup(selected, targets)));
                Refresh();
            }
            catch (AgentRefusedException) { }
            catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
            catch (Exception error)
            {
                if (deviceMenu != menu) return;
                Problem.Message = error.Message;
                Problem.IsOpen = true;
            }
        }
        if (!shown && deviceMenu == menu) deviceMenu = null;

        void AddDeviceAction(DeviceAction action, string title) =>
            AddAction(title, current => DeviceActionTargets.For(current, action) is not null, () => Act(action));

        void AddAction(string title, Func<Device[], bool> available, Action perform)
        {
            var item = new MenuFlyoutItem { Text = title, Visibility = Visibility.Collapsed };
            item.Click += (_, _) =>
            {
                var current = Selected;
                if (IsCurrent(current) && available(current)) perform();
            };
            menu.Items.Add(item);
            actions.Add((item, available));
        }

        bool IsCurrent(Device[] current) => !busy && state is not null && DevicesPage.Visibility == Visibility.Visible &&
            selected.Length == current.Length && selected.All(d => current.Any(d.SameAttachment));

        void Refresh()
        {
            if (deviceMenu != menu) return;
            var current = Selected;
            if (!IsCurrent(current))
            {
                HideDeviceMenu();
                return;
            }
            foreach (var (item, available) in actions)
                item.Visibility = ShowWhen(available(current));
            if (!menu.Items.Any(item => item.Visibility == Visibility.Visible))
            {
                if (shown) HideDeviceMenu();
                return;
            }
            if (shown) return;
            shown = true;
            StateChanged += Refresh;
            if (hasPoint) menu.ShowAt(container, point);
            else menu.ShowAt(container);
        }
    }
}
