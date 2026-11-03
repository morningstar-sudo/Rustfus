// C# side of the rustfus_ffi.dll C ABI (see crates/rustfus-ffi). All results arrive as UTF-8 JSON
// owned by Rust and are always released with rustfus_string_free.
using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

internal sealed class BridgeException : Exception
{
    public readonly int Status;
    public readonly string Code;

    public BridgeException(int status, string code, string message)
        : base(message)
    {
        Status = status;
        Code = code;
    }

    public bool IsClosed
    {
        get { return Status == Bridge.StatusClosed; }
    }
}

internal static class RustfusNative
{
    private const string Dll = "rustfus_ffi.dll";

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr LoadLibraryW(string path);

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int rustfus_abi_version();

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern void rustfus_string_free(IntPtr text);

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int rustfus_enumerate_devices(out IntPtr json);

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int rustfus_analyze_image(
        [MarshalAs(UnmanagedType.LPWStr)] string path, out IntPtr json);

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int rustfus_hash_linux_password(
        [MarshalAs(UnmanagedType.LPWStr)] string password, out IntPtr json);

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int rustfus_session_start(
        [MarshalAs(UnmanagedType.LPStr)] string deviceId,
        [MarshalAs(UnmanagedType.LPStr)] string imageId,
        [MarshalAs(UnmanagedType.LPWStr)] string volumeLabel,
        int verify,
        [MarshalAs(UnmanagedType.LPWStr)] string automationJson,
        [MarshalAs(UnmanagedType.LPWStr)] string confirmation,
        [MarshalAs(UnmanagedType.LPWStr)] string workerPath,
        out IntPtr session,
        out IntPtr json);

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int rustfus_session_next_event(IntPtr session, out IntPtr json);

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern int rustfus_session_cancel(IntPtr session);

    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)]
    public static extern void rustfus_session_close(IntPtr session);
}

internal static class Bridge
{
    public const int StatusClosed = -4;
    private const int ExpectedAbi = 4;

    /// Loads rustfus_ffi.dll from the application directory only (never from PATH or the CWD).
    public static bool TryLoad(out string error)
    {
        error = null;
        try
        {
            string path = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "rustfus_ffi.dll");
            if (!File.Exists(path))
            {
                error = "rustfus_ffi.dll was not found next to the application";
                return false;
            }
            if (RustfusNative.LoadLibraryW(path) == IntPtr.Zero)
            {
                error = "rustfus_ffi.dll could not be loaded (Win32 error " +
                        Marshal.GetLastWin32Error() + ")";
                return false;
            }
            int abi = RustfusNative.rustfus_abi_version();
            if (abi != ExpectedAbi)
            {
                error = "rustfus_ffi.dll ABI " + abi + " is not supported (expected " + ExpectedAbi + ")";
                return false;
            }
            return true;
        }
        catch (Exception ex)
        {
            error = ex.Message;
            return false;
        }
    }

    /// Loads the JSON parser's assembly so the first real response is not delayed by it.
    public static void WarmUp()
    {
        MiniJson.Parse("{\"warm\":[1,\"up\",true,null,1.5]}");
    }

    public static string Str(Dictionary<string, object> map, string key)
    {
        object value;
        return map != null && map.TryGetValue(key, out value) && value != null ? value.ToString() : "";
    }

    public static double Num(Dictionary<string, object> map, string key)
    {
        object value;
        return map != null && map.TryGetValue(key, out value) && value != null
            ? Convert.ToDouble(value)
            : 0.0;
    }

    private static string ReadUtf8(IntPtr pointer)
    {
        int length = 0;
        while (Marshal.ReadByte(pointer, length) != 0) { length++; }
        byte[] bytes = new byte[length];
        Marshal.Copy(pointer, bytes, 0, length);
        return Encoding.UTF8.GetString(bytes);
    }

    /// Converts a status code plus Rust-owned JSON into a dictionary, throwing on failure.
    internal static Dictionary<string, object> Take(int status, IntPtr pointer)
    {
        string text = null;
        if (pointer != IntPtr.Zero)
        {
            try { text = ReadUtf8(pointer); }
            finally { RustfusNative.rustfus_string_free(pointer); }
        }
        Dictionary<string, object> map = text == null
            ? new Dictionary<string, object>()
            : MiniJson.Parse(text) as Dictionary<string, object>;
        if (map == null) { map = new Dictionary<string, object>(); }
        if (status != 0)
        {
            throw new BridgeException(status, Str(map, "code"), Str(map, "message"));
        }
        return map;
    }

    public static List<Dictionary<string, object>> EnumerateDevices()
    {
        IntPtr json;
        int status = RustfusNative.rustfus_enumerate_devices(out json);
        Dictionary<string, object> result = Take(status, json);
        List<Dictionary<string, object>> devices = new List<Dictionary<string, object>>();
        object list;
        if (result.TryGetValue("devices", out list))
        {
            foreach (object item in (object[])list)
            {
                devices.Add((Dictionary<string, object>)item);
            }
        }
        return devices;
    }

    public static Dictionary<string, object> AnalyzeImage(string path)
    {
        IntPtr json;
        int status = RustfusNative.rustfus_analyze_image(path, out json);
        return Take(status, json);
    }

    public static string HashPassword(string password)
    {
        IntPtr json;
        int status = RustfusNative.rustfus_hash_linux_password(password, out json);
        return Str(Take(status, json), "hash");
    }

    /// Launches the worker with the GUI's administrator token, prepares a plan and starts the
    /// operation. Blocks.
    public static BridgeSession StartSession(string deviceId, string imageId, string volumeLabel,
        bool verify, string automationJson, string confirmation, string workerPath)
    {
        IntPtr handle;
        IntPtr json;
        int status = RustfusNative.rustfus_session_start(deviceId, imageId, volumeLabel,
            verify ? 1 : 0, automationJson, confirmation, workerPath, out handle, out json);
        Dictionary<string, object> plan = Take(status, json);
        return new BridgeSession(handle, plan);
    }
}

/// One running operation. Use NextEvent from a single dedicated thread; Cancel may be called from
/// any thread; Dispose only after NextEvent has returned (the event thread owns the lifetime).
internal sealed class BridgeSession : IDisposable
{
    private readonly object gate = new object();
    private IntPtr handle;
    private int active;
    private bool closing;
    public readonly Dictionary<string, object> Plan;

    public BridgeSession(IntPtr handle, Dictionary<string, object> plan)
    {
        this.handle = handle;
        Plan = plan;
    }

    private IntPtr Enter()
    {
        lock (gate)
        {
            if (closing || handle == IntPtr.Zero)
            {
                throw new BridgeException(Bridge.StatusClosed, "io", "The worker session is closed");
            }
            active++;
            return handle;
        }
    }

    private void Leave()
    {
        lock (gate)
        {
            active--;
            Monitor.PulseAll(gate);
        }
    }

    /// Blocks for the next event. A BridgeException with IsClosed means the worker has gone.
    public Dictionary<string, object> NextEvent()
    {
        IntPtr h = Enter();
        try
        {
            IntPtr json;
            int status = RustfusNative.rustfus_session_next_event(h, out json);
            return Bridge.Take(status, json);
        }
        finally { Leave(); }
    }

    public void Cancel()
    {
        IntPtr h = Enter();
        try { Bridge.Take(RustfusNative.rustfus_session_cancel(h), IntPtr.Zero); }
        finally { Leave(); }
    }

    public void Dispose()
    {
        IntPtr h;
        lock (gate)
        {
            if (closing) { return; }
            closing = true;
            while (active > 0) { Monitor.Wait(gate); }
            h = handle;
            handle = IntPtr.Zero;
        }
        if (h != IntPtr.Zero) { RustfusNative.rustfus_session_close(h); }
    }
}
