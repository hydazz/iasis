using Iasis.Agent;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Iasis;

public sealed partial class MainWindow
{
    private async Task AdvanceSetup(DeviceActionTargets targets)
    {
        if (state is null) return;
        var selected = targets.Devices;
        var defaults = DeviceActionTargets.DefaultSetup(targets.Hubs, state.PostRestore, DesktopLanguage);
        var region = defaults.Settings ?? DesktopLanguage;
        var language = new CheckBox
        {
            Content = $"Set language and region ({region.Language}, {region.Locale})",
            IsChecked = defaults.Settings is not null,
        };
        var enroll = new CheckBox { Content = "Try enrollment", IsChecked = defaults.Dep };
        var enrollmentHelp = new TextBlock
        {
            Text = "The device may ask you to sign in or show Partially Set Up. Continue setup on the device.",
            TextWrapping = TextWrapping.Wrap,
        };
        var status = new TextBlock { TextWrapping = TextWrapping.Wrap };
        var content = new StackPanel { Spacing = 8 };
        content.Children.Add(new TextBlock
        {
            Text = selected is [var one] ? $"{one.Place} · {one.Name}" : $"{selected.Count} selected devices",
            TextWrapping = TextWrapping.Wrap,
        });
        content.Children.Add(language);
        content.Children.Add(enroll);
        content.Children.Add(enrollmentHelp);
        content.Children.Add(status);
        var dialog = Dialog("Advance Setup", content, "Advance setup");
        using var closed = CancellationTokenSource.CreateLinkedTokenSource(stop.Token);
        var revision = 0;
        AdvanceSetupRequest? approved = null;
        var available = SelectionAvailable();
        language.Checked += OptionsChanged;
        language.Unchecked += OptionsChanged;
        enroll.Checked += OptionsChanged;
        enroll.Unchecked += OptionsChanged;
        StateChanged += UpdateSelection;
        try
        {
            _ = Check();
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
        }
        finally
        {
            StateChanged -= UpdateSelection;
            closed.Cancel();
        }
        if (!SelectionAvailable() || approved is null || approved != Choice())
            throw new InvalidOperationException("The selection changed. Open Advance Setup again.");
        _ = Track(approved with { CheckOnly = false }, targets);

        AdvanceSetupRequest Choice() => new(targets.Hubs, language.IsChecked == true ? region : null, enroll.IsChecked == true, CheckOnly: true);

        void OptionsChanged(object sender, RoutedEventArgs args) => _ = Check();

        bool SelectionAvailable()
        {
            var current = Selected;
            return state is not null && current.Length == selected.Count && current.All(d => d.CanCheckSetup) &&
                selected.All(d => current.Any(d.SameAttachment));
        }

        void UpdateSelection()
        {
            var next = SelectionAvailable();
            if (available == next) return;
            available = next;
            _ = Check();
        }

        async Task Check()
        {
            var checking = ++revision;
            approved = null;
            dialog.IsPrimaryButtonEnabled = false;
            var request = Choice();
            enrollmentHelp.Visibility = ShowWhen(request.Dep);
            if (!available)
            {
                status.Text = "The selected devices are no longer available for setup.";
                return;
            }
            if (request.Settings is null && !request.Dep)
            {
                status.Text = "Choose at least one setup option.";
                return;
            }
            status.Text = "Checking the selected devices…";
            try
            {
                var reply = await AgentClient.Local.Ask(request, closed.Token);
                if (checking != revision || closed.IsCancellationRequested || !SelectionAvailable()) return;
                if (reply is not OkReply) throw new InvalidDataException("The Iasis service returned an unexpected reply.");
                approved = request;
                status.Text = "";
                dialog.IsPrimaryButtonEnabled = true;
            }
            catch (Exception error)
            {
                if (checking == revision && !closed.IsCancellationRequested) status.Text = error.Message;
            }
        }
    }
}
