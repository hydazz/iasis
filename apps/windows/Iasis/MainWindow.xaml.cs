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
        // What the host lacks shows, with where to get it, until it is
        // installed.
        if (state.Missing.FirstOrDefault() is { } missing)
        {
            Status.Severity = InfoBarSeverity.Warning;
            Status.Message = missing.Reason;
            if ((Status.ActionButton as HyperlinkButton)?.NavigateUri?.OriginalString != missing.Url)
            {
                Status.ActionButton = new HyperlinkButton
                {
                    Content = $"Get {missing.Name}",
                    NavigateUri = new Uri(missing.Url),
                };
            }
            Status.IsOpen = true;
        }
        else
        {
            Status.ActionButton = null;
            Status.IsOpen = false;
        }
        var lacking = state.Missing.ToDictionary(prerequisite => prerequisite.Id);
        Ports.ItemsSource = state.Boards
            .SelectMany(board => board.Ports.Select(port =>
                $"{(board.Name.Length == 0 ? "Iasis" : board.Name)}, port {port.Number}: " +
                (port.Device ?? (port.Occupied ? "Unknown device" : "Empty")) +
                (port.Needs is { } id && lacking.TryGetValue(id, out var needs) ? $", needs {needs.Name}" : "")))
            .ToList();
    }

    private void ShowProblem(string message, InfoBarSeverity severity)
    {
        Status.Severity = severity;
        Status.Message = message;
        Status.ActionButton = null;
        Status.IsOpen = true;
        Ports.ItemsSource = null;
    }
}
