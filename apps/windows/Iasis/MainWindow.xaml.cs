using Iasis.Agent;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace Iasis;

/// Every port of every board the agent attends.
public sealed partial class MainWindow : Window
{
    private readonly CancellationTokenSource stop = new();
    private bool quitting;

    public MainWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        SystemBackdrop = new MicaBackdrop();
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "Iasis.ico"));
        // Closing the window leaves Iasis in the notification area.
        AppWindow.Closing += (_, closing) =>
        {
            if (!quitting)
            {
                closing.Cancel = true;
                AppWindow.Hide();
            }
        };
        _ = Follow();
    }

    public void Quit()
    {
        quitting = true;
        stop.Cancel();
        Close();
    }

    /// Follow the agent's state, and look for the agent again each second
    /// while it is not running.
    private async Task Follow()
    {
        while (!stop.IsCancellationRequested)
        {
            try
            {
                await foreach (var state in AgentClient.Watch(stop.Token))
                {
                    Show(state);
                }
                ShowProblem("The Iasis service stopped.", InfoBarSeverity.Warning);
            }
            catch (AgentRefusedException refused)
            {
                ShowProblem(refused.Message, InfoBarSeverity.Error);
            }
            catch (Exception) when (!stop.IsCancellationRequested)
            {
                ShowProblem("The Iasis service isn’t running.", InfoBarSeverity.Warning);
            }
            try
            {
                await Task.Delay(TimeSpan.FromSeconds(1), stop.Token);
            }
            catch (TaskCanceledException)
            {
                return;
            }
        }
    }

    private void Show(AgentState state)
    {
        Status.IsOpen = false;
        Ports.ItemsSource = state.Boards
            .SelectMany(board => board.Ports.Select(port =>
                $"{(board.Name.Length == 0 ? "Iasis" : board.Name)}, port {port.Number}: " +
                (port.Device ?? (port.Occupied ? "Unknown device" : "Empty"))))
            .ToList();
    }

    private void ShowProblem(string message, InfoBarSeverity severity)
    {
        Status.Severity = severity;
        Status.Message = message;
        Status.IsOpen = true;
        Ports.ItemsSource = null;
    }
}
