using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

[assembly: ComVisible(false)]

namespace ArcoCommander.Automation;

public sealed record NetworkResult(int ExitCode, IReadOnlyList<JsonElement> Events, JsonElement? Failure);

public sealed class NetworkAutomation(string executable, string? profilePath = null)
{
    private const int ScriptLimit = 1024 * 1024;

    public Task<NetworkResult> ListAsync(string profile, string? remotePath = null,
        CancellationToken cancellationToken = default) =>
        RunAsync($"open {Argument(profile)}\n{(remotePath is null ? "" : $"cd {Argument(remotePath)}\n")}ls",
            cancellationToken: cancellationToken);

    public Task<NetworkResult> ListMatchingAsync(string profile, string remotePathOrMask,
        string? passwordVariable = null, CancellationToken cancellationToken = default) =>
        RunAsync($"open {Argument(profile)}\nls {Argument(remotePathOrMask)}",
            passwordVariable, cancellationToken);

    public Task<NetworkResult> UploadAsync(string profile, string localPath, string remotePath,
        CancellationToken cancellationToken = default) =>
        RunAsync($"open {Argument(profile)}\nput {Argument(localPath)} {Argument(remotePath)}",
            cancellationToken: cancellationToken);

    public Task<NetworkResult> DownloadAsync(string profile, string remotePath, string localPath,
        CancellationToken cancellationToken = default) =>
        RunAsync($"open {Argument(profile)}\nget {Argument(remotePath)} {Argument(localPath)}",
            cancellationToken: cancellationToken);

    public Task<NetworkResult> UploadEncryptedAsync(string profile, string localPath, string passphraseVariable,
        string? passwordVariable = null, CancellationToken cancellationToken = default) =>
        RunAsync($"open {Argument(profile)}\nput-encrypted {Argument(localPath)} {Argument(passphraseVariable)}",
            passwordVariable, cancellationToken);

    public Task<NetworkResult> DownloadEncryptedAsync(string profile, string remotePath, string passphraseVariable,
        string? localFolder = null, string? passwordVariable = null, CancellationToken cancellationToken = default)
    {
        var destination = localFolder is null ? "" : $" {Argument(localFolder)}";
        return RunAsync($"open {Argument(profile)}\nget-encrypted {Argument(remotePath)} {Argument(passphraseVariable)}{destination}",
            passwordVariable, cancellationToken);
    }

    public async Task<NetworkResult> RunAsync(string script, string? passwordVariable = null,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(script);
        cancellationToken.ThrowIfCancellationRequested();
        if (Encoding.UTF8.GetByteCount(script) > ScriptLimit)
            throw new ArgumentException("The UTF-8 script exceeds 1 MiB.", nameof(script));

        var start = new ProcessStartInfo(Path.GetFullPath(executable))
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardInputEncoding = new UTF8Encoding(false, true),
            StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8
        };
        start.ArgumentList.Add("--script");
        start.ArgumentList.Add("-");
        if (profilePath is not null)
        {
            start.ArgumentList.Add("--profile");
            start.ArgumentList.Add(Path.GetFullPath(profilePath));
        }
        if (!string.IsNullOrEmpty(passwordVariable))
        {
            start.ArgumentList.Add("--password-env");
            start.ArgumentList.Add(passwordVariable);
        }

        using var process = Process.Start(start) ?? throw new IOException("Could not start arco-net.");
        using var registration = cancellationToken.Register(() => Stop(process));
        var output = process.StandardOutput.ReadToEndAsync(cancellationToken);
        var errors = process.StandardError.ReadToEndAsync(cancellationToken);
        try
        {
            try
            {
                await process.StandardInput.WriteAsync(script.AsMemory(), cancellationToken).ConfigureAwait(false);
                process.StandardInput.Close();
            }
            catch (IOException)
            {
                // The child can close stdin on an argument error before it reads the script.
            }
            await process.WaitForExitAsync(cancellationToken).ConfigureAwait(false);
            var events = Lines(await output.ConfigureAwait(false)).Select(Parse).ToArray();
            var failure = Lines(await errors.ConfigureAwait(false)).SingleOrDefault();
            return new NetworkResult(process.ExitCode, events, failure is null ? null : Parse(failure));
        }
        finally
        {
            Stop(process);
        }
    }

    private static string Argument(string value)
    {
        ArgumentNullException.ThrowIfNull(value);
        if (value.Any(char.IsControl))
            throw new ArgumentException("Script arguments cannot contain control characters.", nameof(value));
        return $"\"{value.Replace("\"", "\"\"")}\"";
    }

    private static string[] Lines(string value) =>
        value.Split('\n', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries);

    private static JsonElement Parse(string value)
    {
        using var document = JsonDocument.Parse(value);
        return document.RootElement.Clone();
    }

    private static void Stop(Process process)
    {
        try
        {
            if (!process.HasExited)
                process.Kill(entireProcessTree: true);
        }
        catch (InvalidOperationException) { }
        catch (System.ComponentModel.Win32Exception) { }
    }
}

[ComVisible(true)]
[Guid("A417FD19-CABA-45E9-B53B-AE4BCC4727D3")]
[InterfaceType(ComInterfaceType.InterfaceIsIDispatch)]
public interface INetworkAutomation
{
    [DispId(1)]
    string Run(string executable, string profilePath, string script, string passwordVariable,
        int timeoutMilliseconds);
}

[ComVisible(true)]
[Guid("94BC3A55-4C2E-4285-9FCE-D7E68E6530CE")]
[ProgId("ArcoCommander.Automation")]
[ClassInterface(ClassInterfaceType.None)]
[ComDefaultInterface(typeof(INetworkAutomation))]
public sealed class NetworkAutomationCom : INetworkAutomation
{
    public string Run(string executable, string profilePath, string script, string passwordVariable,
        int timeoutMilliseconds)
    {
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(timeoutMilliseconds);
        using var cancellation = new CancellationTokenSource(timeoutMilliseconds);
        var result = new NetworkAutomation(executable, string.IsNullOrEmpty(profilePath) ? null : profilePath)
            .RunAsync(script, passwordVariable, cancellation.Token).GetAwaiter().GetResult();
        return JsonSerializer.Serialize(result, JsonSerializerOptions.Web);
    }
}
