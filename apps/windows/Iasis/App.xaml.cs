using CommunityToolkit.Mvvm.Input;
using H.NotifyIcon;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;

namespace Iasis;

/// Iasis lives in the notification area; its window opens from there and
/// closes back to it. A login start (`--background`) shows the icon alone.
public partial class App : Application
{
    private MainWindow? window;
    private TaskbarIcon? tray;

    public App() => InitializeComponent();

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        window = new MainWindow();
        var menu = new MenuFlyout();
        menu.Items.Add(new MenuFlyoutItem { Text = "Open Iasis", Command = new RelayCommand(Show) });
        menu.Items.Add(new MenuFlyoutSeparator());
        menu.Items.Add(new MenuFlyoutItem { Text = "Quit Iasis", Command = new RelayCommand(Quit) });
        tray = new TaskbarIcon
        {
            ToolTipText = "Iasis",
            IconSource = new BitmapImage(new Uri("ms-appx:///Assets/Iasis.ico")),
            LeftClickCommand = new RelayCommand(Show),
            NoLeftClickDelay = true,
            ContextMenuMode = ContextMenuMode.PopupMenu,
            ContextFlyout = menu,
        };
        tray.ForceCreate();
        if (!Environment.GetCommandLineArgs().Contains("--background"))
        {
            Show();
        }
    }

    private void Show() => window?.Activate();

    private void Quit()
    {
        tray?.Dispose();
        window?.Quit();
        Exit();
    }
}
