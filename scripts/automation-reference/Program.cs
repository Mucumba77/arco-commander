using System.Runtime.InteropServices;
using System.Text.Json;
using ArcoCommander.Automation;

var executable = Path.GetFullPath(args[0]);
var manifest = Path.GetFullPath(args[1]);
var sandbox = Directory.CreateTempSubdirectory("arco-automation-test-");
try
{
    var profile = Path.Combine(sandbox.FullName, "profile.json");
    var client = new NetworkAutomation(executable, profile);
    var result = await client.RunAsync("lpwd\nexit");
    Require(result.ExitCode == 0 && result.Events[0].GetProperty("event").GetProperty("local").GetString() is not null,
        "Managed invocation did not return the local directory.");
    var invalid = await client.RunAsync("lpwd\nunknown");
    Require(invalid.ExitCode == 1 && invalid.Events.Count == 0 &&
        invalid.Failure!.Value.GetProperty("error").GetString()!.Contains("Zeile 2"),
        "The CLI must reject the entire script before running it.");
    try
    {
        await client.ListAsync("Name\nrm test");
        throw new Exception("The managed argument accepted a second command.");
    }
    catch (ArgumentException) { }

    using var active = new ComActivation(manifest);
    var type = Type.GetTypeFromCLSID(new Guid("94BC3A55-4C2E-4285-9FCE-D7E68E6530CE"), true)!;
    var instance = Activator.CreateInstance(type)!;
    try
    {
        // Invoke IDispatch through a COM runtime-callable wrapper, not the managed interface.
        var json = (string)((dynamic)instance).Run(executable, profile, "lpwd\nexit", "", 15000);
        using var response = JsonDocument.Parse(json);
        Require(response.RootElement.GetProperty("exitCode").GetInt32() == 0 &&
            response.RootElement.GetProperty("events")[0].GetProperty("event").TryGetProperty("local", out _),
            "Registry-free IDispatch did not return the expected events.");
    }
    finally
    {
        Marshal.FinalReleaseComObject(instance);
    }
    Console.WriteLine("Managed and registry-free COM automation passed.");
}
finally
{
    sandbox.Delete(recursive: true);
}

static void Require(bool condition, string message)
{
    if (!condition)
        throw new Exception(message);
}

sealed class ComActivation : IDisposable
{
    private readonly IntPtr context;
    private readonly nuint cookie;

    public ComActivation(string manifest)
    {
        var info = new ActivationInfo { Size = (uint)Marshal.SizeOf<ActivationInfo>(), Source = manifest };
        context = CreateActCtx(ref info);
        if (context == new IntPtr(-1))
            throw new System.ComponentModel.Win32Exception();
        if (!ActivateActCtx(context, out cookie))
        {
            int error = Marshal.GetLastWin32Error();
            ReleaseActCtx(context);
            throw new System.ComponentModel.Win32Exception(error);
        }
    }

    public void Dispose()
    {
        DeactivateActCtx(0, cookie);
        ReleaseActCtx(context);
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct ActivationInfo
    {
        public uint Size;
        public uint Flags;
        public string Source;
        public ushort ProcessorArchitecture;
        public ushort Language;
        public IntPtr AssemblyDirectory;
        public IntPtr ResourceName;
        public IntPtr ApplicationName;
        public IntPtr Module;
    }

    [DllImport("kernel32.dll", EntryPoint = "CreateActCtxW", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateActCtx(ref ActivationInfo info);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool ActivateActCtx(IntPtr context, out nuint cookie);
    [DllImport("kernel32.dll")]
    private static extern bool DeactivateActCtx(uint flags, nuint cookie);
    [DllImport("kernel32.dll")]
    private static extern void ReleaseActCtx(IntPtr context);
}
