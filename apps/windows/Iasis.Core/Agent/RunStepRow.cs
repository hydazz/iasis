namespace Iasis.Agent;

public sealed record RunStepRow(StepKind Kind, StepState State, string Detail, string Error, string Activity, int? Percent)
{
    public string Title => Device.StepTitle(Kind);
    public string Status => Percent.HasValue ? $"{Percent}%" : Device.StepStatus(State);
    public bool HasDetail => Detail.Length > 0;
    public bool HasError => Error.Length > 0;
    public bool HasActivity => Activity.Length > 0;
    public bool HasProgress => State == StepState.Active && Kind is StepKind.Restore or StepKind.Revive;
    public bool IsIndeterminate => HasProgress && Percent is null;
    public double ProgressValue => Percent ?? 0;

    public static RunStepRow From(OperationStep step)
    {
        var detail = step.Kind switch
        {
            StepKind.Restore or StepKind.Revive when step.System is { } image => $"{image.Os} {image.Version} ({image.Build})",
            StepKind.SetRegion when step.Language is { } region => $"{region.Language} · {region.Locale}",
            _ => "",
        };
        var progress = step is { Kind: StepKind.Restore or StepKind.Revive, State: StepState.Active } ? step.Progress : null;
        return new(step.Kind, step.State, detail, step.Error ?? "", progress?.Name ?? "", progress?.Percent);
    }
}
