// Rustfus desktop front end: WPF on .NET Framework 4.8 (built into Windows 10 22H2 and Windows 11,
// so nothing extra is shipped). All disk and image logic lives in Rust: this program only shows
// state and talks to rustfus_ffi.dll (see Bridge.cs). The window itself is MainWindow.xaml, embedded
// in the executable.
//
// Command line:
//   --self-test   load the Rust library, scan USB devices, check the worker is present, exit.
//   --demo        run with fake data. No disk is touched and nothing is written.
//
// C# 5 only: the compiler that ships with Windows (csc.exe) is the whole toolchain.
using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Markup;
using System.Windows.Media;
using System.Windows.Threading;
using System.Xml;
using Microsoft.Win32;

internal static class Native
{
    [StructLayout(LayoutKind.Sequential)]
    public struct Margins
    {
        public int Left, Right, Top, Bottom;
    }

    [DllImport("dwmapi.dll")]
    public static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int value, int size);

    [DllImport("dwmapi.dll")]
    public static extern int DwmExtendFrameIntoClientArea(IntPtr hwnd, ref Margins margins);

    [DllImport("kernel32.dll")]
    public static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll")]
    public static extern bool SetProcessWorkingSetSize(IntPtr process, IntPtr min, IntPtr max);
}

internal static class Palette
{
    public static readonly string[] Names =
        { "Bg", "Card", "Border", "Text", "Muted", "Accent", "AccentText", "Danger", "Input" };
    private static readonly uint[] Dark =
        { 0x202020, 0x2B2B2B, 0x424242, 0xFFFFFF, 0xA6A6A6, 0x60CDFF, 0x000000, 0xFF99A4, 0x333333 };
    private static readonly uint[] Light =
        { 0xF3F3F3, 0xFFFFFF, 0xE0E0E0, 0x1A1A1A, 0x616161, 0x0067C0, 0xFFFFFF, 0xC42B1C, 0xFBFBFB };

    public static Color Get(int index, bool dark)
    {
        uint c = dark ? Dark[index] : Light[index];
        return Color.FromRgb((byte)(c >> 16), (byte)(c >> 8), (byte)c);
    }

    public static bool SystemIsDark()
    {
        try
        {
            object v = Registry.GetValue(
                @"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "AppsUseLightTheme", 1);
            return v is int && (int)v == 0;
        }
        catch (Exception) { return false; }
    }
}

internal static class Program
{
    private const string UbuntuAutoinstallGeneratorUrl =
        "https://github.com/covertsh/ubuntu-autoinstall-generator/";
    private const int MaximumAutomationConfigBytes = 2 * 1024 * 1024;
    private const int ExitCoreLibraryMissing = 2;
    private const int ExitSelfTestLibrary = 10;
    private const int ExitSelfTestScan = 11;
    private const int ExitSelfTestWorker = 12;

    private static Window win;
    private static bool dark;
    private static bool mica;
    private static bool micaTried;

    private static bool demo;
    private static bool bridgeReady;
    private static string imageId;
    private static bool windowsImage;
    private static bool ubuntuImage;
    private static bool ubuntuAutoinstallSupported;
    private static volatile BridgeSession session;
    private static bool busy;
    private static string lastLoggedPhase = "";
    private static int lastLoggedBucket = -1;

    private static ComboBox device;
    private static TextBox imagePath;
    private static TextBlock mode;
    private static TextBox volume;
    private static CheckBox verify;
    private static Border windowsOptionsCard;
    private static CheckBox unattend;
    private static Grid windowsConfigOptions;
    private static Button configureWindows;
    private static TextBlock windowsConfigSummary;
    private static string windowsGeneratedXml;
    private static Border ubuntuOptionsCard;
    private static CheckBox ubuntuAutoinstall;
    private static StackPanel ubuntuAutoinstallOptions;
    private static RadioButton ubuntuVisualMode;
    private static RadioButton ubuntuYamlMode;
    private static Grid ubuntuVisualOptions;
    private static Grid ubuntuImportOptions;
    private static Button configureUbuntu;
    private static TextBlock ubuntuConfigSummary;
    private static TextBox ubuntuUserDataPath;
    private static TextBox ubuntuMetaDataPath;
    private static Button browseUbuntuUserData;
    private static Button browseUbuntuMetaData;
    private static Button openUbuntuGenerator;
    private static string ubuntuGeneratedUserData;
    private static string ubuntuGeneratedMetaData;
    private static ProgressBar progress;
    private static TextBlock status;
    private static Button start;
    private static Button refresh;
    private static Button browse;
    private static Button analyze;
    private static DispatcherTimer demoTimer;
    private static Task<List<Dictionary<string, object>>> preloadDevices;

    private static T Find<T>(string name) where T : class
    {
        return (T)win.FindName(name);
    }

    private static void Ui(Action action)
    {
        win.Dispatcher.BeginInvoke(action);
    }

    private static bool HasFlag(string[] args, string flag)
    {
        foreach (string arg in args)
        {
            if (string.Equals(arg, flag, StringComparison.OrdinalIgnoreCase)) { return true; }
        }
        return false;
    }

    private static string AppVersion()
    {
        object[] attributes = typeof(Program).Assembly.GetCustomAttributes(
            typeof(System.Reflection.AssemblyInformationalVersionAttribute), false);
        return attributes.Length == 0
            ? "dev"
            : ((System.Reflection.AssemblyInformationalVersionAttribute)attributes[0]).InformationalVersion;
    }

    private static Window LoadMainWindow()
    {
        using (Stream stream = typeof(Program).Assembly.GetManifestResourceStream("Rustfus.MainWindow.xaml"))
        {
            if (stream == null)
            {
                throw new InvalidOperationException("MainWindow.xaml is not embedded in the executable");
            }
            return (Window)XamlReader.Load(stream);
        }
    }

    /// Start-up leaves a lot of garbage (XAML parse, JIT, first scan) and touched pages behind.
    /// Collect once and hand the unused pages back; they are faulted in again only if needed.
    private static void TrimMemory()
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        GC.Collect();
        Native.SetProcessWorkingSetSize(Native.GetCurrentProcess(), new IntPtr(-1), new IntPtr(-1));
    }

    private static void ScheduleTrim()
    {
        win.Dispatcher.BeginInvoke(DispatcherPriority.ApplicationIdle, new Action(TrimMemory));
    }

    private static void ApplyTheme()
    {
        for (int i = 0; i < Palette.Names.Length; i++)
        {
            win.Resources[Palette.Names[i]] = new SolidColorBrush(Palette.Get(i, dark));
        }
        IntPtr h = new WindowInteropHelper(win).Handle;
        if (h == IntPtr.Zero) { return; }
        int v = dark ? 1 : 0;
        Native.DwmSetWindowAttribute(h, 20, ref v, 4);
        Native.DwmSetWindowAttribute(h, 19, ref v, 4);
        if (!micaTried)
        {
            micaTried = true;
            int backdrop = 2;
            mica = Native.DwmSetWindowAttribute(h, 38, ref backdrop, 4) == 0;
            if (mica)
            {
                Native.Margins m = new Native.Margins();
                m.Left = -1; m.Right = -1; m.Top = -1; m.Bottom = -1;
                Native.DwmExtendFrameIntoClientArea(h, ref m);
                HwndSource src = HwndSource.FromHwnd(h);
                if (src != null && src.CompositionTarget != null)
                {
                    src.CompositionTarget.BackgroundColor = Colors.Transparent;
                }
                win.Background = Brushes.Transparent;
            }
        }
    }

    private static string SelectedDeviceId()
    {
        ComboBoxItem item = device.SelectedItem as ComboBoxItem;
        return item == null ? null : item.Tag as string;
    }

    private static string SelectedDeviceText()
    {
        ComboBoxItem item = device.SelectedItem as ComboBoxItem;
        return item == null ? "the selected drive" : Convert.ToString(item.Content);
    }

    private static void UpdateControls()
    {
        bool ready = demo || (imageId != null && SelectedDeviceId() != null);
        start.IsEnabled = !busy && ready;
        device.IsEnabled = !busy;
        refresh.IsEnabled = !busy && !demo;
        browse.IsEnabled = !busy;
        analyze.IsEnabled = !busy;
        imagePath.IsEnabled = !busy;
        volume.IsEnabled = !busy;
        verify.IsEnabled = !busy;
        windowsOptionsCard.Visibility = demo || windowsImage ? Visibility.Visible : Visibility.Collapsed;
        unattend.IsEnabled = !busy && (demo || windowsImage);
        bool unattendedEnabled = unattend.IsEnabled && unattend.IsChecked == true;
        windowsConfigOptions.IsEnabled = unattendedEnabled;
        ubuntuOptionsCard.Visibility = ubuntuImage ? Visibility.Visible : Visibility.Collapsed;
        ubuntuAutoinstall.IsEnabled = !busy && ubuntuImage && ubuntuAutoinstallSupported;
        ubuntuAutoinstallOptions.IsEnabled = ubuntuAutoinstall.IsEnabled &&
                                             ubuntuAutoinstall.IsChecked == true;
        bool visualUbuntu = ubuntuVisualMode.IsChecked == true;
        ubuntuVisualOptions.Visibility = visualUbuntu ? Visibility.Visible : Visibility.Collapsed;
        ubuntuImportOptions.Visibility = visualUbuntu ? Visibility.Collapsed : Visibility.Visible;
    }

    private static string JsonString(string value)
    {
        StringBuilder result = new StringBuilder(value.Length + 2);
        result.Append('"');
        foreach (char character in value)
        {
            switch (character)
            {
                case '"': result.Append("\\\""); break;
                case '\\': result.Append("\\\\"); break;
                case '\b': result.Append("\\b"); break;
                case '\f': result.Append("\\f"); break;
                case '\n': result.Append("\\n"); break;
                case '\r': result.Append("\\r"); break;
                case '\t': result.Append("\\t"); break;
                default:
                    if (char.IsControl(character))
                    {
                        result.Append("\\u");
                        result.Append(((int)character).ToString("x4", CultureInfo.InvariantCulture));
                    }
                    else { result.Append(character); }
                    break;
            }
        }
        result.Append('"');
        return result.ToString();
    }

    private static string ReadAutomationText(string path, string description)
    {
        if (!File.Exists(path))
        {
            throw new InvalidOperationException("Select a valid " + description + " file before starting.");
        }
        FileInfo info = new FileInfo(path);
        if (info.Length > MaximumAutomationConfigBytes * 2L)
        {
            throw new InvalidOperationException(description + " is larger than 2 MiB.");
        }
        try
        {
            string text = File.ReadAllText(path, new UTF8Encoding(false, true));
            if (Encoding.UTF8.GetByteCount(text) > MaximumAutomationConfigBytes)
            {
                throw new InvalidOperationException(description + " is larger than 2 MiB.");
            }
            return text;
        }
        catch (DecoderFallbackException)
        {
            throw new InvalidOperationException(description + " is not valid UTF-8/UTF-16 text.");
        }
    }

    private static bool ValidUbuntuUserData(string text)
    {
        string normalized = text.TrimStart('\uFEFF').Replace("\r\n", "\n").Replace('\r', '\n');
        string[] lines = normalized.Split('\n');
        if (lines.Length == 0 || lines[0].Trim() != "#cloud-config") { return false; }
        foreach (string line in lines)
        {
            if (line.Length == line.TrimStart().Length && line.TrimEnd() == "autoinstall:")
            {
                return true;
            }
        }
        return false;
    }

    private static string BuildAutomationJson()
    {
        if (ubuntuImage)
        {
            if (ubuntuAutoinstall.IsChecked != true) { return ""; }
            string userData;
            string metaData;
            if (ubuntuVisualMode.IsChecked == true)
            {
                if (string.IsNullOrEmpty(ubuntuGeneratedUserData))
                {
                    throw new InvalidOperationException(
                        "Open the full Ubuntu configuration and choose Use this config before starting."
                    );
                }
                userData = ubuntuGeneratedUserData;
                metaData = ubuntuGeneratedMetaData ?? "";
            }
            else
            {
                userData = ReadAutomationText(ubuntuUserDataPath.Text.Trim(), "Ubuntu user-data");
                metaData = ubuntuMetaDataPath.Text.Trim().Length == 0
                    ? ""
                    : ReadAutomationText(ubuntuMetaDataPath.Text.Trim(), "Ubuntu meta-data");
            }
            if (Encoding.UTF8.GetByteCount(userData) + Encoding.UTF8.GetByteCount(metaData) >
                MaximumAutomationConfigBytes)
            {
                throw new InvalidOperationException(
                    "Ubuntu user-data and meta-data together are larger than 2 MiB."
                );
            }
            if (!ValidUbuntuUserData(userData))
            {
                throw new InvalidOperationException(
                    "Ubuntu user-data must start with #cloud-config and contain autoinstall:."
                );
            }
            return "{\"platform\":\"ubuntu\",\"config\":{" +
                "\"user_data\":" + JsonString(userData) + "," +
                "\"meta_data\":" + JsonString(metaData) + "}}";
        }
        if (unattend.IsChecked != true) { return ""; }
        if (string.IsNullOrEmpty(windowsGeneratedXml))
        {
            throw new InvalidOperationException(
                "Open the full Windows configuration and choose Use this config before starting."
            );
        }
        if (Encoding.UTF8.GetByteCount(windowsGeneratedXml) > MaximumAutomationConfigBytes)
        {
            throw new InvalidOperationException("The generated autounattend.xml is larger than 2 MiB.");
        }
        WindowsConfigDialog.ValidateXml(windowsGeneratedXml);
        return "{\"platform\":\"windows\",\"config\":{" +
            "\"custom_xml\":" + JsonString(windowsGeneratedXml) + "}}";
    }

    private static string FormatSize(double bytes)
    {
        string[] units = { "B", "KB", "MB", "GB", "TB" };
        int unit = 0;
        while (bytes >= 1024 && unit < units.Length - 1) { bytes /= 1024; unit++; }
        return bytes.ToString(unit == 0 ? "0" : "0.#") + " " + units[unit];
    }

    private static void FillDemoDevices()
    {
        string[] names = { "E: SanDisk Ultra 32 GB (USB)", "F: Kingston DataTraveler 16 GB (USB)" };
        foreach (string name in names)
        {
            ComboBoxItem item = new ComboBoxItem();
            item.Content = name;
            item.Tag = "demo";
            device.Items.Add(item);
        }
        device.SelectedIndex = 0;
    }

    private static void RefreshDevices()
    {
        if (demo || busy) { return; }
        status.Text = "Scanning USB devices...";
        // The first scan is started in Main, in parallel with building the window; reuse it once.
        Task<List<Dictionary<string, object>>> scan = preloadDevices ??
            Task.Factory.StartNew(() => Bridge.EnumerateDevices());
        preloadDevices = null;
        scan.ContinueWith(task =>
        {
            List<Dictionary<string, object>> devices = null;
            string error = null;
            if (task.IsFaulted) { error = task.Exception.GetBaseException().Message; }
            else { devices = task.Result; }
            Ui(delegate
            {
                device.Items.Clear();
                if (devices != null)
                {
                    List<string> names = new List<string>();
                    foreach (Dictionary<string, object> d in devices)
                    {
                        ComboBoxItem item = new ComboBoxItem();
                        item.Tag = Bridge.Str(d, "id");
                        item.Content = "Disk " + Bridge.Str(d, "disk_number") + ": " +
                                       Bridge.Str(d, "model") + " (" +
                                       FormatSize(Bridge.Num(d, "capacity_bytes")) + ")";
                        device.Items.Add(item);
                        names.Add(Convert.ToString(item.Content));
                    }
                    if (device.Items.Count > 0) { device.SelectedIndex = 0; }
                    status.Text = device.Items.Count > 0
                        ? "Ready"
                        : "No eligible USB device found. System and internal disks are excluded.";
                    SessionLog.Write("USB scan: " + names.Count + " eligible device(s)" +
                                     (names.Count > 0 ? ": " + string.Join("; ", names.ToArray()) : ""));
                }
                else
                {
                    status.Text = "USB scan failed: " + error;
                    SessionLog.Write("USB scan failed: " + error);
                }
                UpdateControls();
                ScheduleTrim();
            });
        });
    }

    private static void AnalyzeImage()
    {
        string path = imagePath.Text.Trim();
        if (path.Length == 0) { return; }
        if (demo)
        {
            mode.Text = "Windows 11 x64 installation media (demo)";
            return;
        }
        imageId = null;
        windowsImage = false;
        ubuntuImage = false;
        ubuntuAutoinstallSupported = false;
        mode.Text = "Analyzing and fingerprinting the ISO...";
        SessionLog.Write("Analyzing image " + path);
        UpdateControls();
        ThreadPool.QueueUserWorkItem(delegate
        {
            Dictionary<string, object> image = null;
            string error = null;
            try { image = Bridge.AnalyzeImage(path); }
            catch (Exception ex) { error = ex.Message; }
            Ui(delegate
            {
                if (image != null)
                {
                    imageId = Bridge.Str(image, "id");
                    string imageKind = Bridge.Str(image, "kind");
                    windowsImage = imageKind == "windows_installer";
                    ubuntuImage = imageKind == "ubuntu_installer";
                    ubuntuAutoinstallSupported = ubuntuImage &&
                                                   Bridge.Num(image, "required_bytes") > 0;
                    if (!windowsImage) { unattend.IsChecked = false; }
                    if (!ubuntuAutoinstallSupported) { ubuntuAutoinstall.IsChecked = false; }
                    string kind = windowsImage ? "Windows installation media"
                        : ubuntuImage ? "Ubuntu installation media"
                        : "Hybrid bootable image";
                    mode.Text = kind + " - " + FormatSize(Bridge.Num(image, "size_bytes"));
                    string warningText = "";
                    object warnings;
                    if (image.TryGetValue("warnings", out warnings) && ((object[])warnings).Length > 0)
                    {
                        warningText = string.Join("; ", Array.ConvertAll((object[])warnings, w => w.ToString()));
                        status.Text = warningText;
                    }
                    SessionLog.Write("Image analyzed: " + kind + ", " +
                                     FormatSize(Bridge.Num(image, "size_bytes")) + ", id " + imageId +
                                     (warningText.Length > 0 ? ", warnings: " + warningText : ""));
                }
                else
                {
                    mode.Text = "Unsupported or invalid image: " + error;
                    SessionLog.Write("Image rejected: " + error);
                }
                UpdateControls();
            });
        });
    }

    /// Asks the user to confirm erasing the selected drive. The default answer is No.
    private static bool ConfirmErase()
    {
        string file = imagePath.Text.Trim().Length == 0
            ? "the selected image"
            : System.IO.Path.GetFileName(imagePath.Text.Trim());
        string message = "All data on this USB drive will be permanently erased:\n\n    " +
                         SelectedDeviceText() + "\n\nand replaced with:\n\n    " + file +
                         "\n\nThis cannot be undone. Do you want to continue?";
        return MessageBox.Show(win, message, "Erase USB drive?", MessageBoxButton.YesNo,
            MessageBoxImage.Warning, MessageBoxResult.No) == MessageBoxResult.Yes;
    }

    private static void StartOperation()
    {
        string automationConfig;
        try { automationConfig = BuildAutomationJson(); }
        catch (Exception ex)
        {
            status.Text = ex.Message;
            if (ubuntuImage)
            {
                if (ubuntuVisualMode.IsChecked == true) { configureUbuntu.Focus(); }
                else { browseUbuntuUserData.Focus(); }
            }
            else { configureWindows.Focus(); }
            return;
        }
        if (!ConfirmErase())
        {
            SessionLog.Write("Erase declined by the user");
            return;
        }
        if (demo)
        {
            progress.Value = 0;
            demoTimer.Start();
            return;
        }
        string deviceId = SelectedDeviceId();
        string image = imageId;
        string label = volume.Text;
        bool verifyReadBack = verify.IsChecked == true;
        // The user has just answered the Yes/No prompt above; the worker protocol still requires
        // its confirmation phrase, which the worker checks again.
        const string confirmation = "ERASE";
        string worker = System.IO.Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "rustfus-worker.exe");
        SessionLog.Write("Starting operation: device [" + SelectedDeviceText() + "], image [" +
                         imagePath.Text.Trim() + "], label [" + label + "], verify " + verifyReadBack +
                         ", installation automation " + (automationConfig.Length > 0));
        lastLoggedPhase = "";
        lastLoggedBucket = -1;
        busy = true;
        progress.Value = 0;
        status.Text = "Waiting for elevation and writing the USB media...";
        UpdateControls();
        Thread thread = new Thread(delegate()
        {
            RunOperation(deviceId, image, label, verifyReadBack, automationConfig, confirmation, worker);
        });
        thread.IsBackground = true;
        thread.Start();
    }

    private static void RunOperation(string deviceId, string image, string label, bool verifyReadBack,
        string automationConfig, string confirmation, string worker)
    {
        BridgeSession s = null;
        try
        {
            s = Bridge.StartSession(deviceId, image, label, verifyReadBack, automationConfig,
                                    confirmation, worker);
            session = s;
            SessionLog.Write("Worker accepted the plan: mode " + Bridge.Str(s.Plan, "mode") + ", " +
                             FormatSize(Bridge.Num(s.Plan, "bytes_to_write")) + " to write");
            while (true)
            {
                Dictionary<string, object> ev = s.NextEvent();
                Ui(delegate { ShowEvent(ev); });
                object terminal;
                if (ev.TryGetValue("terminal", out terminal) && terminal is bool && (bool)terminal) { break; }
            }
        }
        catch (BridgeException ex)
        {
            if (!ex.IsClosed)
            {
                SessionLog.Write("Operation failed: " + ex.Message);
                Ui(delegate { status.Text = "Failed: " + ex.Message; });
            }
        }
        catch (Exception ex)
        {
            SessionLog.Write("Operation failed: " + ex.Message);
            Ui(delegate { status.Text = "Failed: " + ex.Message; });
        }
        finally
        {
            session = null;
            if (s != null) { s.Dispose(); }
            Ui(delegate
            {
                busy = false;
                UpdateControls();
            });
        }
    }

    private static void ShowEvent(Dictionary<string, object> ev)
    {
        string kind = Bridge.Str(ev, "event");
        switch (kind)
        {
            case "progress":
                string phase = Bridge.Str(ev, "phase");
                double total = Bridge.Num(ev, "total_bytes");
                double percent = total <= 0 ? 0 : Bridge.Num(ev, "completed_bytes") * 100.0 / total;
                string speed = (Bridge.Num(ev, "bytes_per_second") / 1048576.0).ToString("0.0");
                progress.Value = percent;
                status.Text = phase + ": " + percent.ToString("0.0") + "% (" + speed + " MiB/s)";
                int bucket = (int)(percent / 5.0);
                if (phase != lastLoggedPhase || bucket != lastLoggedBucket)
                {
                    lastLoggedPhase = phase;
                    lastLoggedBucket = bucket;
                    SessionLog.Write(phase + ": " + percent.ToString("0.0") + "% (" + speed + " MiB/s)");
                }
                break;
            case "phase_changed":
                status.Text = "Phase: " + Bridge.Str(ev, "phase");
                SessionLog.Write("Entered phase " + Bridge.Str(ev, "phase"));
                break;
            case "log":
                status.Text = Bridge.Str(ev, "message");
                SessionLog.Write(Bridge.Str(ev, "message"));
                break;
            case "completed":
                progress.Value = 100;
                status.Text = "Operation completed successfully";
                SessionLog.Write("Operation completed successfully");
                break;
            case "cancelled":
                status.Text = "Operation cancelled; media may be incomplete";
                SessionLog.Write("Operation cancelled; media may be incomplete");
                break;
            case "failed":
                Dictionary<string, object> error = ev.ContainsKey("error")
                    ? ev["error"] as Dictionary<string, object>
                    : null;
                status.Text = "Operation failed: " + Bridge.Str(error, "message");
                SessionLog.Write("Operation failed: " + Bridge.Str(error, "code") + ": " +
                                 Bridge.Str(error, "message"));
                break;
        }
    }

    private static void CancelOperation()
    {
        BridgeSession s = session;
        if (s != null)
        {
            try
            {
                s.Cancel();
                status.Text = "Cancellation requested...";
                SessionLog.Write("Cancellation requested");
            }
            catch (BridgeException ex)
            {
                if (!ex.IsClosed) { status.Text = "Cancel failed: " + ex.Message; }
            }
            return;
        }
        if (demo)
        {
            demoTimer.Stop();
            progress.Value = 0;
            status.Text = "Cancelled";
        }
    }

    private static void ExportLog()
    {
        string current = SessionLog.Path;
        if (current == null)
        {
            status.Text = "No log is available for this session.";
            return;
        }
        SaveFileDialog dialog = new SaveFileDialog();
        dialog.Filter = "Log file (*.log)|*.log|All files (*.*)|*.*";
        dialog.FileName = System.IO.Path.GetFileName(current);
        if (dialog.ShowDialog(win) != true) { return; }
        string error = SessionLog.Export(dialog.FileName);
        status.Text = error == null
            ? "Log exported to " + dialog.FileName
            : "Could not export the log: " + error;
    }

    /// The single .iso file in a drag-and-drop payload, or null.
    private static string DroppedIso(IDataObject data)
    {
        if (!data.GetDataPresent(DataFormats.FileDrop)) { return null; }
        string[] files = data.GetData(DataFormats.FileDrop) as string[];
        if (files == null || files.Length != 1) { return null; }
        return string.Equals(System.IO.Path.GetExtension(files[0]), ".iso", StringComparison.OrdinalIgnoreCase)
            ? files[0]
            : null;
    }

    private static int SelfTest()
    {
        SessionLog.Start("Rustfus " + AppVersion() + " self-test");
        string error;
        if (!Bridge.TryLoad(out error))
        {
            SessionLog.Write("Self-test failed: core library: " + error);
            return ExitSelfTestLibrary;
        }
        try { Bridge.EnumerateDevices(); }
        catch (Exception ex)
        {
            SessionLog.Write("Self-test failed: USB scan: " + ex.Message);
            return ExitSelfTestScan;
        }
        try
        {
            string hash = Bridge.HashPassword("rustfus-self-test");
            if (!hash.StartsWith("$6$", StringComparison.Ordinal))
            {
                throw new InvalidOperationException("Ubuntu password hashing returned an unexpected format");
            }
        }
        catch (Exception ex)
        {
            SessionLog.Write("Self-test failed: Ubuntu password hashing: " + ex.Message);
            return ExitSelfTestLibrary;
        }
        try
        {
            Window owner = LoadMainWindow();
            UbuntuConfigDialog ubuntuDialog = new UbuntuConfigDialog(owner, null, null);
            ubuntuDialog.Close();
            WindowsConfigDialog windowsDialog = new WindowsConfigDialog(owner, null);
            WindowsConfigDialog.ValidateXml(windowsDialog.GeneratedXml);
            if (windowsDialog.GeneratedXml.IndexOf("Microsoft-Windows-Setup", StringComparison.Ordinal) < 0 ||
                windowsDialog.GeneratedXml.IndexOf("Microsoft-Windows-Shell-Setup", StringComparison.Ordinal) < 0)
            {
                throw new InvalidOperationException("Windows editor omitted required setup components");
            }
            windowsDialog.Close();
            owner.Close();
        }
        catch (Exception ex)
        {
            SessionLog.Write("Self-test failed: automation configuration dialogs: " + ex.Message);
            return ExitSelfTestLibrary;
        }
        string worker = System.IO.Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "rustfus-worker.exe");
        if (!File.Exists(worker))
        {
            SessionLog.Write("Self-test failed: worker not found at " + worker);
            return ExitSelfTestWorker;
        }
        SessionLog.Write("Self-test passed");
        return 0;
    }

    [STAThread]
    private static int Main(string[] args)
    {
        if (HasFlag(args, "--self-test")) { return SelfTest(); }
        demo = HasFlag(args, "--demo");

        SessionLog.Start("Rustfus " + AppVersion() + " started" +
                         (demo ? " in demo mode (no disk is touched)" : ""));
        string loadError = null;
        bridgeReady = !demo && Bridge.TryLoad(out loadError);
        if (!demo && !bridgeReady)
        {
            SessionLog.Write("Cannot start: " + loadError);
            MessageBox.Show(
                "Rustfus cannot start because its core library could not be loaded:\n\n" + loadError +
                "\n\nExtract the complete release package and run Rustfus.exe from there.",
                "Rustfus", MessageBoxButton.OK, MessageBoxImage.Error);
            return ExitCoreLibraryMissing;
        }
        if (bridgeReady)
        {
            // Start the first USB scan and load the JSON parser while the window is being built, so
            // neither delays the first device list.
            preloadDevices = Task.Factory.StartNew(() =>
            {
                Bridge.WarmUp();
                return Bridge.EnumerateDevices();
            });
        }

        // The UI is nearly static, so GPU rendering buys nothing; software rendering avoids the GPU
        // driver's committed memory (measured: private bytes 66 -> 48 MB, window ~30 ms sooner).
        RenderOptions.ProcessRenderMode = RenderMode.SoftwareOnly;

        win = LoadMainWindow();
        if (demo) { win.Title += " (demo: no disk is touched)"; }
        dark = Palette.SystemIsDark();
        ApplyTheme();
        win.SourceInitialized += delegate { ApplyTheme(); };

        device = Find<ComboBox>("device");
        imagePath = Find<TextBox>("imagePath");
        mode = Find<TextBlock>("mode");
        volume = Find<TextBox>("volume");
        verify = Find<CheckBox>("verify");
        windowsOptionsCard = Find<Border>("windowsOptionsCard");
        unattend = Find<CheckBox>("unattend");
        windowsConfigOptions = Find<Grid>("windowsConfigOptions");
        configureWindows = Find<Button>("configureWindows");
        windowsConfigSummary = Find<TextBlock>("windowsConfigSummary");
        ubuntuOptionsCard = Find<Border>("ubuntuOptionsCard");
        ubuntuAutoinstall = Find<CheckBox>("ubuntuAutoinstall");
        ubuntuAutoinstallOptions = Find<StackPanel>("ubuntuAutoinstallOptions");
        ubuntuVisualMode = Find<RadioButton>("ubuntuVisualMode");
        ubuntuYamlMode = Find<RadioButton>("ubuntuYamlMode");
        ubuntuVisualOptions = Find<Grid>("ubuntuVisualOptions");
        ubuntuImportOptions = Find<Grid>("ubuntuImportOptions");
        configureUbuntu = Find<Button>("configureUbuntu");
        ubuntuConfigSummary = Find<TextBlock>("ubuntuConfigSummary");
        ubuntuUserDataPath = Find<TextBox>("ubuntuUserDataPath");
        ubuntuMetaDataPath = Find<TextBox>("ubuntuMetaDataPath");
        browseUbuntuUserData = Find<Button>("browseUbuntuUserData");
        browseUbuntuMetaData = Find<Button>("browseUbuntuMetaData");
        openUbuntuGenerator = Find<Button>("openUbuntuGenerator");
        progress = Find<ProgressBar>("progress");
        status = Find<TextBlock>("status");
        start = Find<Button>("start");
        refresh = Find<Button>("refresh");
        browse = Find<Button>("browse");
        analyze = Find<Button>("analyze");

        Find<Button>("themeToggle").Click += delegate { dark = !dark; ApplyTheme(); };
        Find<Button>("exportLog").Click += delegate { ExportLog(); };
        refresh.Click += delegate { RefreshDevices(); };
        analyze.Click += delegate { AnalyzeImage(); };
        unattend.Checked += delegate { UpdateControls(); };
        unattend.Unchecked += delegate { UpdateControls(); };
        ubuntuAutoinstall.Checked += delegate { UpdateControls(); };
        ubuntuAutoinstall.Unchecked += delegate { UpdateControls(); };
        ubuntuVisualMode.Checked += delegate { UpdateControls(); };
        ubuntuVisualMode.Unchecked += delegate { UpdateControls(); };
        ubuntuYamlMode.Checked += delegate { UpdateControls(); };
        ubuntuYamlMode.Unchecked += delegate { UpdateControls(); };
        configureUbuntu.Click += delegate
        {
            UbuntuConfigDialog dialog = new UbuntuConfigDialog(
                win, ubuntuGeneratedUserData, ubuntuGeneratedMetaData);
            if (dialog.ShowDialog() == true)
            {
                ubuntuGeneratedUserData = dialog.UserData;
                ubuntuGeneratedMetaData = dialog.MetaData;
                ubuntuConfigSummary.Text = "Configured: " +
                    FormatSize(Encoding.UTF8.GetByteCount(ubuntuGeneratedUserData)) +
                    " user-data; reopen the editor to review or edit it";
                status.Text = "Ubuntu full visual configuration is ready.";
            }
        };
        configureWindows.Click += delegate
        {
            WindowsConfigDialog dialog = new WindowsConfigDialog(win, windowsGeneratedXml);
            if (dialog.ShowDialog() == true)
            {
                windowsGeneratedXml = dialog.GeneratedXml;
                windowsConfigSummary.Text = "Configured: " +
                    FormatSize(Encoding.UTF8.GetByteCount(windowsGeneratedXml)) +
                    " autounattend.xml; reopen the editor to review it or change options";
                status.Text = "Windows full visual configuration is ready.";
            }
        };
        openUbuntuGenerator.Click += delegate
        {
            try
            {
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(
                    UbuntuAutoinstallGeneratorUrl) { UseShellExecute = true });
            }
            catch (Exception ex)
            {
                status.Text = "Could not open the Ubuntu generator project: " + ex.Message;
            }
        };
        browseUbuntuUserData.Click += delegate
        {
            OpenFileDialog dialog = new OpenFileDialog();
            dialog.Filter = "Cloud-init user-data (user-data)|user-data|YAML files (*.yaml;*.yml)|*.yaml;*.yml|All files (*.*)|*.*";
            if (dialog.ShowDialog(win) == true)
            {
                ubuntuUserDataPath.Text = dialog.FileName;
                status.Text = "Ubuntu user-data selected.";
            }
        };
        browseUbuntuMetaData.Click += delegate
        {
            OpenFileDialog dialog = new OpenFileDialog();
            dialog.Filter = "Cloud-init meta-data (meta-data)|meta-data|YAML files (*.yaml;*.yml)|*.yaml;*.yml|All files (*.*)|*.*";
            if (dialog.ShowDialog(win) == true)
            {
                ubuntuMetaDataPath.Text = dialog.FileName;
                status.Text = "Ubuntu meta-data selected.";
            }
        };
        browse.Click += delegate
        {
            OpenFileDialog dialog = new OpenFileDialog();
            dialog.Filter = "ISO image (*.iso)|*.iso|All files (*.*)|*.*";
            if (dialog.ShowDialog(win) == true)
            {
                imagePath.Text = dialog.FileName;
                AnalyzeImage();
            }
        };
        device.SelectionChanged += delegate { UpdateControls(); };
        start.Click += delegate { StartOperation(); };
        Find<Button>("cancel").Click += delegate { CancelOperation(); };

        // Dropping an ISO anywhere on the window selects and analyzes it. Preview events run before
        // the text box gets to treat the drop as text.
        win.PreviewDragOver += delegate(object sender, DragEventArgs e)
        {
            e.Effects = !busy && DroppedIso(e.Data) != null ? DragDropEffects.Copy : DragDropEffects.None;
            e.Handled = true;
        };
        win.PreviewDrop += delegate(object sender, DragEventArgs e)
        {
            e.Handled = true;
            if (busy) { return; }
            string iso = DroppedIso(e.Data);
            if (iso == null)
            {
                status.Text = "Drop a single .iso image file.";
                return;
            }
            imagePath.Text = iso;
            AnalyzeImage();
        };

        demoTimer = new DispatcherTimer();
        demoTimer.Interval = TimeSpan.FromMilliseconds(80);
        demoTimer.Tick += delegate
        {
            progress.Value = Math.Min(progress.Value + 1, 100);
            status.Text = "Writing (demo)... " + (int)progress.Value + "%";
            if (progress.Value >= 100) { demoTimer.Stop(); }
        };

        win.Closing += delegate(object sender, System.ComponentModel.CancelEventArgs e)
        {
            if (busy)
            {
                e.Cancel = true;
                CancelOperation();
                status.Text = "An operation is running; cancelling. Close again once it has stopped.";
            }
        };

        device.Items.Clear();
        if (demo)
        {
            FillDemoDevices();
            status.Text = "Demo mode: fake devices and no disk access.";
            win.ContentRendered += delegate { ScheduleTrim(); };
        }
        else
        {
            status.Text = "Scanning USB devices...";
            win.ContentRendered += delegate { RefreshDevices(); };
        }
        UpdateControls();

        int exitCode = new Application().Run(win);
        SessionLog.Write("Rustfus exited");
        return exitCode;
    }
}
