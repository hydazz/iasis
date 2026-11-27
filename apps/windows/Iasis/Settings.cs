using System.Globalization;
using System.Security.Cryptography;
using Iasis.Agent;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.Win32;
using Microsoft.Windows.Storage.Pickers;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;

namespace Iasis;

public sealed partial class MainWindow
{
    private const string LoginPath = @"Software\Microsoft\Windows\CurrentVersion\Run";

    private static Agent.SetupLanguage DesktopLanguage => new(
        CultureInfo.CurrentUICulture.Name, CultureInfo.CurrentCulture.Name.Replace('-', '_'));

    private void LoginChanged(object sender, RoutedEventArgs args)
    {
        if (!initialized) return;
        try
        {
            using var keys = Registry.CurrentUser.CreateSubKey(LoginPath);
            if (Login.IsOn) keys.SetValue("Iasis", $"\"{Environment.ProcessPath}\" --background");
            else keys.DeleteValue("Iasis", throwOnMissingValue: false);
        }
        catch (Exception error)
        {
            initialized = false;
            Login.IsOn = !Login.IsOn;
            initialized = true;
            Problem.Message = error.Message;
            Problem.IsOpen = true;
        }
    }

    private void ShowPostRestore()
    {
        ShutdownMacs.IsEnabled = state is not null && !busy;
        ShutdownMacs.IsOn = state?.PostRestore.ShutdownMacs == true;
        var region = state?.PostRestore.SetupLanguage;
        SetupLanguage.IsEnabled = state is not null && !busy;
        SetupLanguage.IsOn = region is not null;
        SetupLanguage.OnContent = region is null ? "On" : $"{region.Language}, {region.Locale}";
        EnrollmentAfterErase.IsEnabled = state is not null && !busy;
        EnrollmentAfterErase.IsOn = state?.PostRestore.TryEnrollment == true;
    }

    private void ShutdownMacsChanged(object sender, RoutedEventArgs args)
    {
        var enabled = ShutdownMacs.IsOn;
        if (initialized && state is not null && enabled != state.PostRestore.ShutdownMacs)
            _ = Run(() => Send(new ShutdownMacsRequest(enabled)));
    }

    private void SetupLanguageChanged(object sender, RoutedEventArgs args)
    {
        var enabled = SetupLanguage.IsOn;
        if (initialized && state is not null && enabled != (state.PostRestore.SetupLanguage is not null))
        {
            var settings = enabled ? DesktopLanguage : null;
            _ = Run(() => Send(new SetupLanguageRequest(settings)));
        }
    }

    private void EnrollmentAfterEraseChanged(object sender, RoutedEventArgs args)
    {
        var enabled = EnrollmentAfterErase.IsOn;
        if (initialized && state is not null && enabled != state.PostRestore.TryEnrollment)
            _ = Run(() => Send(new EnrollmentAfterEraseRequest(enabled)));
    }

    private Agent.Hub? CurrentHub => (HubList.SelectedItem as HubRow)?.Hub;

    private void HubChanged(object sender, SelectionChangedEventArgs args)
    {
        if (initialized) ShowHub();
    }

    private void ShowHub()
    {
        var hub = CurrentHub;
        Rename.IsEnabled = Brightness.IsEnabled = !busy && hub?.Session is not null;
        Update.IsEnabled = !busy && hub?.Update is not null;
        if (hub is null)
        {
            HubStatus.Text = "";
            return;
        }
        var lines = new List<string>();
        if (!string.IsNullOrEmpty(hub.Firmware)) lines.Add($"IasisOS {hub.Firmware}");
        lines.Add($"Light level {hub.Brightness}%");
        if (hub.Update is { } update) lines.Add($"IasisOS {update.Version} {(update.Required ? "required" : "available")}");
        if (hub.Notice is { } notice) lines.Add(notice);
        HubStatus.Text = string.Join("\n", lines);
    }

    private void RenameHub(object sender, RoutedEventArgs args) => _ = Run(async () =>
    {
        if (CurrentHub is not { } hub) return;
        var name = new TextBox { Header = "Hub name (up to 16 printable ASCII characters)", Text = hub.Name, MaxLength = 16 };
        if (await Dialog($"Rename {Device.HubTitle(hub)}", name, "Save").ShowAsync() == ContentDialogResult.Primary)
            await Send(new RenameRequest(hub.Serial, name.Text.Trim()));
    });

    private void ChangeBrightness(object sender, RoutedEventArgs args) => _ = Run(async () =>
    {
        if (CurrentHub is not { } hub) return;
        var level = new Slider { Header = "Light level (%)", Minimum = 0, Maximum = 100, Value = hub.Brightness, StepFrequency = 1 };
        if (await Dialog(Device.HubTitle(hub), level, "Apply").ShowAsync() == ContentDialogResult.Primary)
            await Send(new BrightnessRequest(hub.Serial, (int)level.Value));
    });

    private void UpdateHub(object sender, RoutedEventArgs args) => _ = Run(async () =>
    {
        if (CurrentHub is not { Update: { } update } hub) return;
        if (await Dialog("Update IasisOS?", $"Install IasisOS {update.Version} on {Device.HubTitle(hub)} ({hub.Serial})? The hub will restart.", "Update")
            .ShowAsync() == ContentDialogResult.Primary)
            await Send(new UpdateRequest(hub.Serial, null));
    });

    private void ShowIdentity()
    {
        var identity = state?.Supervision;
        IdentityName.Text = identity?.Problem ?? identity?.Name ?? "No supervision identity";
        Fingerprint.Text = identity?.Certificate is { } cert
            ? BitConverter.ToString(SHA256.HashData(Convert.FromBase64String(cert))).Replace('-', ':') : "";
        CertificateExport.IsEnabled = IdentityExport.IsEnabled = FingerprintCopy.IsEnabled = !busy && Fingerprint.Text.Length > 0;
        IdentityImport.IsEnabled = state is not null && !busy;
    }

    private void CopyFingerprint(object sender, RoutedEventArgs args) => _ = Run(() =>
    {
        var data = new DataPackage();
        data.SetText(Fingerprint.Text);
        Clipboard.SetContent(data);
        return Task.CompletedTask;
    });

    private void ExportCertificate(object sender, RoutedEventArgs args) => _ = Run(async () =>
    {
        if (state?.Supervision.Certificate is { } certificate)
            await SaveIdentityFile("Certificate", ".cer", Convert.FromBase64String(certificate));
    });

    private void ExportIdentity(object sender, RoutedEventArgs args) => _ = Run(async () =>
    {
        var password = new PasswordBox { Header = "Protect the private key with a password" };
        var confirm = new PasswordBox { Header = "Confirm password" };
        var content = new StackPanel { Spacing = 8, Children = { password, confirm } };
        var dialog = Dialog("Export supervision identity", content, "Export");
        dialog.IsPrimaryButtonEnabled = false;
        void Validate(object sender, RoutedEventArgs e) =>
            dialog.IsPrimaryButtonEnabled = password.Password.Length > 0 && password.Password == confirm.Password;
        password.PasswordChanged += Validate;
        confirm.PasswordChanged += Validate;
        if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
        if (await AgentClient.Local.Ask(new ExportSupervisionRequest(password.Password), stop.Token) is not SupervisionReply identity)
            throw new InvalidDataException("The service returned no supervision identity.");
        await SaveIdentityFile("PKCS #12 archive", ".p12", Convert.FromBase64String(identity.Pkcs12));
    });

    private async Task SaveIdentityFile(string type, string extension, byte[] bytes)
    {
        var picker = new FileSavePicker(AppWindow.Id) { SuggestedFileName = "Iasis" };
        picker.FileTypeChoices.Add(type, [extension]);
        if (await picker.PickSaveFileAsync() is { } result)
            await File.WriteAllBytesAsync(result.Path, bytes, stop.Token);
    }

    private void ImportIdentity(object sender, RoutedEventArgs args) => _ = Run(async () =>
    {
        var picker = new FileOpenPicker(AppWindow.Id);
        picker.FileTypeFilter.Add(".p12");
        picker.FileTypeFilter.Add(".pfx");
        if (await picker.PickSingleFileAsync() is not { } result) return;
        var password = new PasswordBox { Header = "Archive password" };
        var content = new StackPanel { Spacing = 8 };
        content.Children.Add(new TextBlock
        {
            Text = $"Replace the host’s supervision identity with {Path.GetFileName(result.Path)}?",
            TextWrapping = TextWrapping.Wrap,
        });
        content.Children.Add(password);
        if (await Dialog("Import supervision identity", content, "Replace identity").ShowAsync() != ContentDialogResult.Primary) return;
        var archive = await File.ReadAllBytesAsync(result.Path, stop.Token);
        await Send(new ImportSupervisionRequest(Convert.ToBase64String(archive), password.Password));
    });

    private void ShowImages()
    {
        var store = state?.Images;
        foreach (var removed in images.Where(row => store?.Images.Any(i => i.Id == row.Image.Id) != true).ToArray())
            images.Remove(removed);
        foreach (var image in store?.Images ?? [])
        {
            var row = images.FirstOrDefault(r => r.Image.Id == image.Id);
            if (row is null) images.Add(new ImageRow(image));
            else row.Update(image);
        }
        NoImages.Visibility = ShowWhen(state is not null && images.Count == 0);
        DownloadProgress.Visibility = ShowWhen(store?.Fetch is not null);
        if (store?.Fetch is { } fetch)
        {
            FetchTitle.Text = $"{fetch.Id} · {DeviceActionTargets.Size(fetch.Done)} of {DeviceActionTargets.Size(fetch.Bytes)}";
            FetchProgress.IsIndeterminate = fetch.Bytes == 0;
            FetchProgress.Value = fetch.Bytes == 0 ? 0 : 100.0 * fetch.Done / fetch.Bytes;
        }
        ImageButtons();
    }

    private void ImageButtons()
    {
        Import.IsEnabled = Download.IsEnabled = state is not null && !busy;
        Remove.IsEnabled = ImageList.SelectedItem is not null && !busy;
    }

    private void ImageSelectionChanged(object sender, SelectionChangedEventArgs args)
    {
        if (initialized) ImageButtons();
    }

    private void ImportImage(object sender, RoutedEventArgs args) => _ = Run(async () =>
    {
        var picker = new FileOpenPicker(AppWindow.Id);
        picker.FileTypeFilter.Add(".ipsw");
        if (await picker.PickSingleFileAsync() is not { } file) return;
        await using var source = new FileStream(file.Path, FileMode.Open, FileAccess.Read, FileShare.Read, 81920, useAsync: true);
        await Send(new ImportRequest(Path.GetFileName(file.Path), source.Length), source);
    });

    private void DownloadImage(object sender, RoutedEventArgs args) => _ = Run(async () =>
    {
        if (await AgentClient.Local.Ask(new CatalogRequest(), stop.Token) is not CatalogReply catalog)
            throw new InvalidDataException("The service returned no model catalog.");
        var models = new ListView { MaxHeight = 320, SelectionMode = ListViewSelectionMode.Single };
        var search = new TextBox { PlaceholderText = "Search by model name or identifier" };
        var content = new StackPanel { Spacing = 8, Children = { search, models } };
        void FilterModels()
        {
            models.ItemsSource = catalog.Models.Where(m => m.Name.Contains(search.Text, StringComparison.OrdinalIgnoreCase) ||
                m.Identifier.Contains(search.Text, StringComparison.OrdinalIgnoreCase))
                .Select(m => new PickerChoice<CatalogModel>($"{m.Name} ({m.Identifier})", m)).ToArray();
        }
        FilterModels();
        search.TextChanged += (_, _) => FilterModels();
        var choose = Dialog("Choose a device model", content, "Next");
        choose.IsPrimaryButtonEnabled = false;
        models.SelectionChanged += (_, _) => choose.IsPrimaryButtonEnabled = models.SelectedItem is not null;
        if (await choose.ShowAsync() != ContentDialogResult.Primary || models.SelectedItem is not PickerChoice<CatalogModel> selection) return;
        if (await AgentClient.Local.Ask(new ReleasesRequest(selection.Value.Identifier), stop.Token) is not ReleasesReply releases || releases.Releases.Count == 0)
            throw new InvalidDataException($"No signed releases are available for {selection.Value.Name}.");
        var choices = new ListView
        {
            MaxHeight = 320,
            ItemsSource = releases.Releases.Select(r => new PickerChoice<Release>(
                $"{DeviceActionTargets.ReleaseName(r)} · {DeviceActionTargets.Size(r.Bytes)}", r)).ToArray(),
            SelectedIndex = 0,
        };
        if (await Dialog(selection.Value.Name, choices, "Download").ShowAsync() == ContentDialogResult.Primary &&
            choices.SelectedItem is PickerChoice<Release> release)
            await Send(new DownloadRequest(selection.Value.Identifier, release.Value.Build));
    });

    private sealed record PickerChoice<T>(string Label, T Value)
    {
        public override string ToString() => Label;
    }

    private void RemoveImage(object sender, RoutedEventArgs args) => RemoveRow(ImageList.SelectedItem as ImageRow);
    private void RemoveContextImage(object sender, RoutedEventArgs args) => RemoveRow(((MenuFlyoutItem)sender).Tag as ImageRow);
    private void SwipeRemoveImage(SwipeItem sender, SwipeItemInvokedEventArgs args) => RemoveRow(args.SwipeControl.DataContext as ImageRow);

    private void ImageKeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (args.Key == VirtualKey.Delete)
        {
            args.Handled = true;
            RemoveRow(ImageList.SelectedItem as ImageRow);
        }
    }

    private void RemoveRow(ImageRow? row) => _ = Run(async () =>
    {
        if (row is null) return;
        var verb = row.Image.Ready ? "Remove" : "Cancel download";
        if (await Dialog($"{verb}?", row.Detail, verb).ShowAsync() == ContentDialogResult.Primary)
            await Send(new RemoveRequest(row.Image.Id));
    });
}
