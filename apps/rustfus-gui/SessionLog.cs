// Per-session log under %LOCALAPPDATA%\Rustfus\logs. Logging must never get in the way of the
// application, so every I/O failure is swallowed and simply turns logging off.
using System;
using System.IO;
using System.Text;

internal static class SessionLog
{
    private const int KeepDays = 30;
    private static readonly object Gate = new object();
    private static readonly Encoding Utf8 = new UTF8Encoding(false);
    private static string path;

    /// Full path of the current log file, or null when logging is unavailable.
    public static string Path
    {
        get { lock (Gate) { return path; } }
    }

    public static string Directory
    {
        get
        {
            return System.IO.Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "Rustfus", "logs");
        }
    }

    /// Creates the log file for this run and removes logs older than 30 days.
    public static void Start(string headline)
    {
        try
        {
            string directory = Directory;
            System.IO.Directory.CreateDirectory(directory);
            Prune(directory);
            string file = System.IO.Path.Combine(directory,
                "rustfus-" + DateTime.Now.ToString("yyyyMMdd-HHmmss") + ".log");
            lock (Gate) { path = file; }
            Write(headline);
        }
        catch (Exception ex)
        {
            if (!IsIoFailure(ex)) { throw; }
            lock (Gate) { path = null; }
        }
    }

    public static void Write(string message)
    {
        lock (Gate)
        {
            if (path == null) { return; }
            try
            {
                string line = DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss.fff") + "  " + message +
                              Environment.NewLine;
                File.AppendAllText(path, line, Utf8);
            }
            catch (Exception ex)
            {
                if (!IsIoFailure(ex)) { throw; }
                path = null;
            }
        }
    }

    /// Copies the current log to `destination`. Returns an error message, or null on success.
    public static string Export(string destination)
    {
        string source = Path;
        if (source == null || !File.Exists(source)) { return "There is no log for this session."; }
        try
        {
            lock (Gate) { File.Copy(source, destination, true); }
            return null;
        }
        catch (Exception ex)
        {
            if (!IsIoFailure(ex)) { throw; }
            return ex.Message;
        }
    }

    private static void Prune(string directory)
    {
        DateTime cutoff = DateTime.Now.AddDays(-KeepDays);
        foreach (string file in System.IO.Directory.GetFiles(directory, "rustfus-*.log"))
        {
            try
            {
                if (File.GetLastWriteTime(file) < cutoff) { File.Delete(file); }
            }
            catch (Exception ex)
            {
                if (!IsIoFailure(ex)) { throw; }
            }
        }
    }

    private static bool IsIoFailure(Exception ex)
    {
        return ex is IOException || ex is UnauthorizedAccessException ||
               ex is System.Security.SecurityException || ex is NotSupportedException ||
               ex is ArgumentException;
    }
}
