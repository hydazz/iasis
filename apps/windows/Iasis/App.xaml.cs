using CommunityToolkit.Mvvm.Input;
using H.NotifyIcon;
using Iasis.Agent;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;

namespace Iasis;

public partial class App : Application
{
    private MainWindow? window;
    private TaskbarIcon? tray;

    public App() => InitializeComponent();

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        window = new MainWindow();
        tray = new TaskbarIcon
        {
            ToolTipText = "Iasis",
            IconSource = new BitmapImage(new Uri("ms-appx:///Assets/Iasis.ico")),
            LeftClickCommand = new RelayCommand(() => window.Open()),
            NoLeftClickDelay = true,
            ContextMenuMode = ContextMenuMode.PopupMenu,
        };
        window.StateChanged += UpdateMenu;
        UpdateMenu();
        tray.ForceCreate();
        if (!Environment.GetCommandLineArgs().Contains("--background")) window.Open();
    }

    private void UpdateMenu()
    {
        if (window is not { } main || tray is null) return;
        var menu = new MenuFlyout();
        foreach (var hub in main.State?.Hubs ?? [])
        {
            var section = new MenuFlyoutSubItem { Text = string.IsNullOrEmpty(hub.Name) ? hub.Serial : $"{hub.Name} ({hub.Serial})" };
            foreach (var d in main.Devices.Where(d => d.Hub.Serial == hub.Serial && (d.Port.Occupied || d.Port.Run?.Finished() == false)))
            {
                var target = new MenuFlyoutSubItem { Text = $"{d.Port.Number}. {d.Name}" + (d.Status.Length > 0 ? $" · {d.Status}" : "") };
                AddAction("Enter DFU", DeviceAction.Dfu);
                AddAction("Restart", DeviceAction.Restart);
                AddAction("Revive", DeviceAction.Revive);
                AddAction("Restore…", DeviceAction.Restore);
                section.Items.Add(target.Items.Count == 0 ? new MenuFlyoutItem { Text = target.Text, IsEnabled = false } : target);

                void AddAction(string title, DeviceAction action)
                {
                    if (DeviceActionTargets.For([d], action) is null) return;
                    target.Items.Add(new MenuFlyoutItem
                    {
                        Text = title,
                        Command = new RelayCommand(() =>
                        {
                            main.Open();
                            main.Act(action, d);
                        }),
                    });
                }
            }
            if (section.Items.Count == 0) section.Items.Add(new MenuFlyoutItem { Text = hub.Notice ?? "No devices", IsEnabled = false });
            menu.Items.Add(section);
        }
        if (main.State?.Images.Fetch is { } fetch)
        {
            var percent = fetch.Bytes == 0 ? "" : $" · {100.0 * fetch.Done / fetch.Bytes:0}%";
            menu.Items.Add(new MenuFlyoutItem { Text = $"Downloading {fetch.Id}{percent}", IsEnabled = false });
        }
        if (menu.Items.Count > 0) menu.Items.Add(new MenuFlyoutSeparator());
        menu.Items.Add(new MenuFlyoutItem { Text = "Open Iasis", Command = new RelayCommand(() => main.Open()) });
        menu.Items.Add(new MenuFlyoutItem { Text = "Settings", Command = new RelayCommand(() => main.Open(settings: true)) });
        menu.Items.Add(new MenuFlyoutItem { Text = "Quit Iasis", Command = new RelayCommand(Quit) });
        tray.ContextFlyout = menu;
    }

    private void Quit()
    {
        tray?.Dispose();
        window?.Quit();
        Exit();
    }
}
