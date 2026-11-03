using System;
using System.Collections.Generic;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;

internal sealed class UbuntuConfigDialog : Window
{
    private readonly CheckBox includeIdentity = Check("Create the primary user account", true);
    private readonly TextBox realName = Box(Environment.UserName);
    private readonly TextBox username = Box(SafeUserName());
    private readonly TextBox hostname = Box("ubuntu");
    private readonly PasswordBox password = new PasswordBox { Height = 32, Padding = new Thickness(7, 5, 7, 5) };
    private readonly TextBox groups = Box("adm,sudo");
    private readonly TextBox locale = Box("en_US.UTF-8");
    private readonly TextBox timezone = Box("Etc/UTC");
    private readonly TextBox keyboardLayout = Box("us");
    private readonly TextBox keyboardVariant = Box("");
    private readonly TextBox keyboardToggle = Box("");

    private readonly TextBox network = Multi("");
    private readonly TextBox storage = Multi("layout:\n  name: direct");
    private readonly TextBox apt = Multi("");
    private readonly TextBox proxy = Box("");
    private readonly TextBox ubuntuPro = Box("");
    private readonly CheckBox geoip = Check("Use GeoIP mirror selection", true);

    private readonly CheckBox installSsh = Check("Install OpenSSH server", true);
    private readonly CheckBox allowPasswordSsh = Check("Allow SSH password authentication", false);
    private readonly TextBox sshKeys = Multi("");
    private readonly TextBox packages = Multi("openssh-server");
    private readonly TextBox snaps = Multi("");
    private readonly CheckBox installDrivers = Check("Install third-party drivers", false);
    private readonly CheckBox installCodecs = Check("Install codecs", false);
    private readonly ComboBox updates = Choices("security", "all");
    private readonly ComboBox shutdown = Choices("reboot", "poweroff", "omit");
    private readonly ComboBox oem = Choices("omit", "auto", "true", "false");

    private readonly CheckBox refreshInstaller = Check("Refresh Subiquity before installation", false);
    private readonly TextBox refreshChannel = Box("stable/ubuntu-$REL");
    private readonly TextBox kernelPackage = Box("");
    private readonly TextBox kernelFlavor = Box("");
    private readonly ComboBox crashDumps = Choices("omit", "auto", "true", "false");
    private readonly TextBox sourceId = Box("");
    private readonly CheckBox searchDrivers = Check("Search for drivers in the selected source", true);
    private readonly TextBox activeDirectoryDomain = Box("");
    private readonly TextBox activeDirectoryAdmin = Box("");

    private readonly TextBox interactiveSections = Multi("");
    private readonly TextBox earlyCommands = Multi("");
    private readonly TextBox lateCommands = Multi("");
    private readonly TextBox errorCommands = Multi("");
    private readonly TextBox reporting = Multi("");
    private readonly TextBox targetUserData = Multi("");
    private readonly TextBox debconfSelections = Multi("");
    private readonly TextBox zdevs = Multi("");
    private readonly TextBox extraOptions = Multi("");

    private readonly TextBox preview = Multi("");
    private readonly TextBox metaData = Multi("");
    private readonly TextBlock validation = new TextBlock { Margin = new Thickness(0, 8, 0, 0), TextWrapping = TextWrapping.Wrap };
    private bool updatingPreview;
    private bool previewEdited;
    private bool generationSucceeded;

    public string UserData { get; private set; }
    public string MetaData { get; private set; }

    public UbuntuConfigDialog(Window owner, string existingUserData, string existingMetaData)
    {
        Owner = owner;
        Title = "Ubuntu autoinstall configuration";
        Width = 920;
        Height = 760;
        MinWidth = 760;
        MinHeight = 600;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        Background = ResourceBrush(owner, "Bg", Brushes.White);
        Foreground = ResourceBrush(owner, "Text", Brushes.Black);

        Grid root = new Grid { Margin = new Thickness(18) };
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        TabControl tabs = new TabControl();
        tabs.Items.Add(Tab("Identity & regional", IdentityPanel()));
        tabs.Items.Add(Tab("Network & storage", NetworkPanel()));
        tabs.Items.Add(Tab("Software & access", SoftwarePanel()));
        tabs.Items.Add(Tab("Installer", InstallerPanel()));
        tabs.Items.Add(Tab("Commands & advanced", AdvancedPanel()));
        tabs.Items.Add(Tab("Generated YAML", PreviewPanel()));
        root.Children.Add(tabs);

        Grid buttons = new Grid { Margin = new Thickness(0, 14, 0, 0) };
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Button generate = new Button { Content = "Generate / refresh YAML", MinWidth = 180, Height = 34, Margin = new Thickness(0, 0, 8, 0) };
        Button cancel = new Button { Content = "Cancel", Width = 100, Height = 34, Margin = new Thickness(0, 0, 8, 0), IsCancel = true };
        Button use = new Button { Content = "Use this config", Width = 140, Height = 34, IsDefault = true };
        generate.Click += delegate { GenerateYaml(); tabs.SelectedIndex = 5; };
        use.Click += delegate { Accept(); };
        Grid.SetColumn(generate, 0);
        Grid.SetColumn(cancel, 1);
        Grid.SetColumn(use, 2);
        buttons.Children.Add(generate);
        buttons.Children.Add(cancel);
        buttons.Children.Add(use);
        Grid.SetRow(buttons, 1);
        root.Children.Add(buttons);
        Content = root;

        preview.TextChanged += delegate
        {
            if (!updatingPreview) { previewEdited = true; }
        };

        updates.SelectedIndex = 0;
        shutdown.SelectedIndex = 0;
        oem.SelectedIndex = 0;
        crashDumps.SelectedIndex = 0;
        GenerateYaml();
        if (!string.IsNullOrEmpty(existingUserData))
        {
            updatingPreview = true;
            preview.Text = existingUserData;
            updatingPreview = false;
            previewEdited = true;
            metaData.Text = existingMetaData ?? "";
            validation.Text = "Loaded the current configuration. Edit the YAML directly, or change form fields and regenerate it.";
            tabs.SelectedIndex = 5;
        }
    }

    private UIElement IdentityPanel()
    {
        StackPanel panel = Form();
        panel.Children.Add(Heading("Primary account"));
        panel.Children.Add(includeIdentity);
        AddField(panel, "Real name", realName, "Optional display name.");
        AddField(panel, "Username", username, "Linux account name.");
        AddField(panel, "Hostname", hostname, "Installed machine hostname.");
        AddField(panel, "Password", password, "Plain text is converted locally to a salted SHA-512 crypt hash before it enters YAML.");
        AddField(panel, "Groups", groups, "Comma-separated supplementary groups.");
        panel.Children.Add(Heading("Regional settings"));
        AddField(panel, "Locale", locale, "Example: en_US.UTF-8 or vi_VN.UTF-8.");
        AddField(panel, "Time zone", timezone, "IANA name, for example Asia/Ho_Chi_Minh.");
        AddField(panel, "Keyboard layout", keyboardLayout, "XKB layout, for example us or vn.");
        AddField(panel, "Keyboard variant", keyboardVariant, "Optional XKB variant.");
        AddField(panel, "Keyboard toggle", keyboardToggle, "Optional layout-toggle shortcut.");
        return Scroll(panel);
    }

    private UIElement NetworkPanel()
    {
        StackPanel panel = Form();
        panel.Children.Add(Heading("Network"));
        AddFragment(panel, "network", network,
            "Netplan mapping below the network key. Leave blank for installer defaults. Supports ethernets, Wi-Fi, bonds, bridges, VLANs and tunnels.");
        AddField(panel, "HTTP/HTTPS proxy", proxy, "Optional URI used by the installer.");
        panel.Children.Add(Heading("Storage"));
        AddFragment(panel, "storage", storage,
            "Storage mapping below the storage key. Use layout/name for guided modes or config for complete curtin partitioning, RAID, LVM and encryption.");
        panel.Children.Add(Heading("APT and Ubuntu Pro"));
        AddFragment(panel, "apt", apt,
            "APT mapping: mirrors, sources, components, preferences and fallback policy.");
        panel.Children.Add(geoip);
        AddField(panel, "Ubuntu Pro token", ubuntuPro, "Optional contract token. It is stored in user-data on the USB.");
        return Scroll(panel);
    }

    private UIElement SoftwarePanel()
    {
        StackPanel panel = Form();
        panel.Children.Add(Heading("Remote access"));
        panel.Children.Add(installSsh);
        panel.Children.Add(allowPasswordSsh);
        AddField(panel, "Authorized SSH keys", sshKeys, "One public key per line.");
        panel.Children.Add(Heading("Packages and snaps"));
        AddField(panel, "APT packages", packages, "One package per line.");
        AddFragment(panel, "snaps", snaps, "YAML list below the snaps key. Each item supports name, channel and classic.");
        panel.Children.Add(Heading("Hardware and updates"));
        panel.Children.Add(installDrivers);
        panel.Children.Add(installCodecs);
        AddField(panel, "Updates", updates, "Install security-only or all package updates.");
        AddField(panel, "OEM metapackages", oem, "Omit, auto-detect, enable or disable OEM installation.");
        AddField(panel, "After installation", shutdown, "Reboot or power off after setup completes.");
        return Scroll(panel);
    }

    private UIElement InstallerPanel()
    {
        StackPanel panel = Form();
        panel.Children.Add(Heading("Subiquity and source"));
        panel.Children.Add(refreshInstaller);
        AddField(panel, "Refresh channel", refreshChannel, "Snap channel used when refresh is enabled.");
        AddField(panel, "Source ID", sourceId, "Optional source/capability ID from the ISO.");
        panel.Children.Add(searchDrivers);
        panel.Children.Add(Heading("Kernel"));
        AddField(panel, "Kernel package", kernelPackage, "Specify either a package or a flavor, not both.");
        AddField(panel, "Kernel flavor", kernelFlavor, "Example: generic, hwe or lowlatency.");
        AddField(panel, "Kernel crash dumps", crashDumps, "Omit, automatic (null), enabled or disabled.");
        panel.Children.Add(Heading("Active Directory"));
        AddField(panel, "Domain name", activeDirectoryDomain, "Optional domain joined by the installed machine.");
        AddField(panel, "Administrator name", activeDirectoryAdmin, "Optional Active Directory administrator.");
        return Scroll(panel);
    }

    private UIElement AdvancedPanel()
    {
        StackPanel panel = Form();
        panel.Children.Add(Heading("Installer interaction and hooks"));
        AddField(panel, "Interactive sections", interactiveSections, "One section name per line; use * to make every section interactive.");
        AddField(panel, "Early commands", earlyCommands, "One shell command or inline YAML command array per line.");
        AddField(panel, "Late commands", lateCommands, "One shell command or inline YAML command array per line.");
        AddField(panel, "Error commands", errorCommands, "One shell command or inline YAML command array per line.");
        panel.Children.Add(Heading("Open-schema sections"));
        AddFragment(panel, "reporting", reporting, "Reporting mapping below the reporting key.");
        AddFragment(panel, "user-data", targetUserData, "Cloud-init mapping applied to the installed target system.");
        AddField(panel, "Debconf selections", debconfSelections, "Raw debconf-set-selections text.");
        AddFragment(panel, "zdevs", zdevs, "YAML list of IBM Z devices below the zdevs key.");
        AddFragment(panel, "Additional autoinstall keys", extraOptions,
            "Raw keys placed directly below autoinstall. Use this for new upstream keys not yet represented above.");
        return Scroll(panel);
    }

    private UIElement PreviewPanel()
    {
        Grid panel = new Grid { Margin = new Thickness(8) };
        panel.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        panel.RowDefinitions.Add(new RowDefinition { Height = new GridLength(3, GridUnitType.Star) });
        panel.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        panel.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        panel.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        TextBlock userLabel = Heading("Complete user-data (editable)");
        panel.Children.Add(userLabel);
        preview.AcceptsReturn = true;
        preview.AcceptsTab = true;
        preview.VerticalScrollBarVisibility = ScrollBarVisibility.Auto;
        preview.HorizontalScrollBarVisibility = ScrollBarVisibility.Auto;
        preview.FontFamily = new FontFamily("Consolas");
        Grid.SetRow(preview, 1);
        panel.Children.Add(preview);
        TextBlock metaLabel = Heading("NoCloud meta-data (editable)");
        metaLabel.Margin = new Thickness(0, 10, 0, 4);
        Grid.SetRow(metaLabel, 2);
        panel.Children.Add(metaLabel);
        metaData.AcceptsReturn = true;
        metaData.FontFamily = new FontFamily("Consolas");
        Grid.SetRow(metaData, 3);
        panel.Children.Add(metaData);
        Grid.SetRow(validation, 4);
        panel.Children.Add(validation);
        return panel;
    }

    private void GenerateYaml()
    {
        generationSucceeded = false;
        try
        {
            if (kernelPackage.Text.Trim().Length > 0 && kernelFlavor.Text.Trim().Length > 0)
            {
                throw new InvalidOperationException("Choose a kernel package or a kernel flavor, not both.");
            }
            StringBuilder yaml = new StringBuilder();
            yaml.AppendLine("#cloud-config");
            yaml.AppendLine("autoinstall:");
            yaml.AppendLine("  version: 1");
            if (includeIdentity.IsChecked == true)
            {
                Require(username.Text, "Username");
                Require(hostname.Text, "Hostname");
                string hash = password.Password.Length == 0 ? "!" : Bridge.HashPassword(password.Password);
                yaml.AppendLine("  identity:");
                AppendScalar(yaml, 4, "realname", realName.Text);
                AppendScalar(yaml, 4, "username", username.Text);
                AppendScalar(yaml, 4, "hostname", hostname.Text);
                AppendScalar(yaml, 4, "password", hash);
                AppendInlineList(yaml, 4, "groups", groups.Text);
            }
            AppendScalar(yaml, 2, "locale", locale.Text);
            AppendScalar(yaml, 2, "timezone", timezone.Text);
            if (keyboardLayout.Text.Trim().Length > 0)
            {
                yaml.AppendLine("  keyboard:");
                AppendScalar(yaml, 4, "layout", keyboardLayout.Text);
                AppendScalar(yaml, 4, "variant", keyboardVariant.Text);
                AppendScalar(yaml, 4, "toggle", keyboardToggle.Text);
            }
            AppendFragment(yaml, 2, "network", network.Text);
            AppendFragment(yaml, 2, "storage", storage.Text);
            AppendScalar(yaml, 2, "proxy", proxy.Text);
            if (ubuntuPro.Text.Trim().Length > 0)
            {
                yaml.AppendLine("  ubuntu-pro:");
                AppendScalar(yaml, 4, "token", ubuntuPro.Text);
            }
            if (apt.Text.Trim().Length > 0 || geoip.IsChecked == false)
            {
                yaml.AppendLine("  apt:");
                if (geoip.IsChecked == false) { yaml.AppendLine("    geoip: false"); }
                AppendRawLines(yaml, 4, apt.Text);
            }
            yaml.AppendLine("  ssh:");
            yaml.AppendLine("    install-server: " + Bool(installSsh));
            yaml.AppendLine("    allow-pw: " + Bool(allowPasswordSsh));
            AppendList(yaml, 4, "authorized-keys", sshKeys.Text, false);
            AppendList(yaml, 2, "packages", packages.Text, false);
            AppendFragment(yaml, 2, "snaps", snaps.Text);
            yaml.AppendLine("  drivers:");
            yaml.AppendLine("    install: " + Bool(installDrivers));
            yaml.AppendLine("  codecs:");
            yaml.AppendLine("    install: " + Bool(installCodecs));
            AppendPlain(yaml, 2, "updates", Selected(updates));
            if (Selected(shutdown) != "omit") { AppendPlain(yaml, 2, "shutdown", Selected(shutdown)); }
            if (Selected(oem) != "omit")
            {
                yaml.AppendLine("  oem:");
                AppendPlain(yaml, 4, "install", Selected(oem));
            }
            if (refreshInstaller.IsChecked == true)
            {
                yaml.AppendLine("  refresh-installer:");
                yaml.AppendLine("    update: true");
                AppendScalar(yaml, 4, "channel", refreshChannel.Text);
            }
            if (sourceId.Text.Trim().Length > 0 || searchDrivers.IsChecked == false)
            {
                yaml.AppendLine("  source:");
                AppendScalar(yaml, 4, "id", sourceId.Text);
                yaml.AppendLine("    search_drivers: " + Bool(searchDrivers));
            }
            if (kernelPackage.Text.Trim().Length > 0 || kernelFlavor.Text.Trim().Length > 0)
            {
                yaml.AppendLine("  kernel:");
                AppendScalar(yaml, 4, "package", kernelPackage.Text);
                AppendScalar(yaml, 4, "flavor", kernelFlavor.Text);
            }
            if (Selected(crashDumps) != "omit")
            {
                yaml.AppendLine("  kernel-crash-dumps:");
                string value = Selected(crashDumps) == "auto" ? "null" : Selected(crashDumps);
                AppendPlain(yaml, 4, "enabled", value);
            }
            if (activeDirectoryDomain.Text.Trim().Length > 0 || activeDirectoryAdmin.Text.Trim().Length > 0)
            {
                yaml.AppendLine("  active-directory:");
                AppendScalar(yaml, 4, "domain-name", activeDirectoryDomain.Text);
                AppendScalar(yaml, 4, "admin-name", activeDirectoryAdmin.Text);
            }
            AppendList(yaml, 2, "interactive-sections", interactiveSections.Text, false);
            AppendList(yaml, 2, "early-commands", earlyCommands.Text, true);
            AppendList(yaml, 2, "late-commands", lateCommands.Text, true);
            AppendList(yaml, 2, "error-commands", errorCommands.Text, true);
            AppendFragment(yaml, 2, "reporting", reporting.Text);
            AppendFragment(yaml, 2, "user-data", targetUserData.Text);
            if (debconfSelections.Text.Trim().Length > 0)
            {
                yaml.AppendLine("  debconf-selections: |");
                AppendRawLines(yaml, 4, debconfSelections.Text);
            }
            AppendFragment(yaml, 2, "zdevs", zdevs.Text);
            AppendRawLines(yaml, 2, extraOptions.Text);
            updatingPreview = true;
            preview.Text = yaml.ToString();
            updatingPreview = false;
            previewEdited = false;
            metaData.Text = "instance-id: rustfus-" + DateTime.UtcNow.ToString("yyyyMMddHHmmss") + "\n" +
                            "local-hostname: " + PlainOrQuoted(hostname.Text.Trim()) + "\n";
            validation.Text = "Generated from every tab. You can edit the complete YAML before using it.";
            generationSucceeded = true;
        }
        catch (Exception ex)
        {
            updatingPreview = false;
            validation.Text = ex.Message;
        }
    }

    private void Accept()
    {
        if (!previewEdited)
        {
            GenerateYaml();
            if (!generationSucceeded) { return; }
        }
        string text = preview.Text.Trim();
        if (!text.StartsWith("#cloud-config", StringComparison.Ordinal) ||
            text.IndexOf("\nautoinstall:", StringComparison.Ordinal) < 0)
        {
            validation.Text = "user-data must start with #cloud-config and contain a top-level autoinstall: key.";
            return;
        }
        if (Encoding.UTF8.GetByteCount(preview.Text) + Encoding.UTF8.GetByteCount(metaData.Text) > 2 * 1024 * 1024)
        {
            validation.Text = "user-data and meta-data together exceed 2 MiB.";
            return;
        }
        UserData = preview.Text;
        MetaData = metaData.Text;
        DialogResult = true;
    }

    private static string Bool(CheckBox box) { return box.IsChecked == true ? "true" : "false"; }
    private static string Selected(ComboBox box) { return Convert.ToString(box.SelectedItem); }
    private static void Require(string value, string name)
    {
        if (string.IsNullOrWhiteSpace(value)) { throw new InvalidOperationException(name + " is required."); }
    }
    private static string SafeUserName()
    {
        string value = Environment.UserName.ToLowerInvariant();
        StringBuilder clean = new StringBuilder();
        foreach (char c in value)
        {
            if ((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-' || c == '_') { clean.Append(c); }
        }
        return clean.Length == 0 || !(clean[0] >= 'a' && clean[0] <= 'z') ? "ubuntu" : clean.ToString();
    }
    private static string Quote(string value) { return "'" + value.Replace("'", "''") + "'"; }
    private static string PlainOrQuoted(string value) { return value.Length == 0 ? "''" : Quote(value); }
    private static void AppendScalar(StringBuilder yaml, int indent, string key, string value)
    {
        AppendScalar(yaml, indent, key, value, true);
    }
    private static void AppendScalar(StringBuilder yaml, int indent, string key, string value, bool omitBlank)
    {
        if (omitBlank && string.IsNullOrWhiteSpace(value)) { return; }
        yaml.Append(' ', indent).Append(key).Append(": ").AppendLine(Quote(value.Trim()));
    }
    private static void AppendPlain(StringBuilder yaml, int indent, string key, string value)
    {
        yaml.Append(' ', indent).Append(key).Append(": ").AppendLine(value);
    }
    private static void AppendInlineList(StringBuilder yaml, int indent, string key, string csv)
    {
        string[] values = csv.Split(new[] { ',' }, StringSplitOptions.RemoveEmptyEntries);
        if (values.Length == 0) { return; }
        yaml.Append(' ', indent).Append(key).Append(": [");
        for (int index = 0; index < values.Length; index++)
        {
            if (index > 0) { yaml.Append(", "); }
            yaml.Append(Quote(values[index].Trim()));
        }
        yaml.AppendLine("]");
    }
    private static void AppendList(StringBuilder yaml, int indent, string key, string text, bool allowInlineYaml)
    {
        List<string> lines = Lines(text);
        if (lines.Count == 0) { return; }
        yaml.Append(' ', indent).Append(key).AppendLine(":");
        foreach (string line in lines)
        {
            string value = allowInlineYaml && line.StartsWith("[", StringComparison.Ordinal) ? line : Quote(line);
            yaml.Append(' ', indent + 2).Append("- ").AppendLine(value);
        }
    }
    private static void AppendFragment(StringBuilder yaml, int indent, string key, string text)
    {
        if (string.IsNullOrWhiteSpace(text)) { return; }
        yaml.Append(' ', indent).Append(key).AppendLine(":");
        AppendRawLines(yaml, indent + 2, text);
    }
    private static void AppendRawLines(StringBuilder yaml, int indent, string text)
    {
        string normalized = text.Replace("\r\n", "\n").Replace('\r', '\n').Trim('\n');
        if (normalized.Length == 0) { return; }
        foreach (string line in normalized.Split('\n'))
        {
            yaml.Append(' ', indent).AppendLine(line);
        }
    }
    private static List<string> Lines(string text)
    {
        List<string> result = new List<string>();
        foreach (string line in text.Replace("\r\n", "\n").Replace('\r', '\n').Split('\n'))
        {
            if (line.Trim().Length > 0) { result.Add(line.Trim()); }
        }
        return result;
    }

    private static StackPanel Form() { return new StackPanel { Margin = new Thickness(8) }; }
    private static TextBox Box(string text) { return new TextBox { Text = text, Height = 32, Padding = new Thickness(7, 5, 7, 5) }; }
    private static TextBox Multi(string text)
    {
        return new TextBox
        {
            Text = text,
            MinHeight = 86,
            AcceptsReturn = true,
            AcceptsTab = true,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            FontFamily = new FontFamily("Consolas"),
            Padding = new Thickness(7)
        };
    }
    private static CheckBox Check(string text, bool value)
    {
        return new CheckBox { Content = text, IsChecked = value, Margin = new Thickness(0, 6, 0, 6) };
    }
    private static ComboBox Choices(params string[] values)
    {
        ComboBox box = new ComboBox { Height = 32 };
        foreach (string value in values) { box.Items.Add(value); }
        box.SelectedIndex = 0;
        return box;
    }
    private static TextBlock Heading(string text)
    {
        return new TextBlock { Text = text, FontSize = 16, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 12, 0, 8) };
    }
    private static void AddField(Panel panel, string label, Control control, string help)
    {
        panel.Children.Add(new TextBlock { Text = label, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 8, 0, 4) });
        panel.Children.Add(control);
        panel.Children.Add(new TextBlock { Text = help, Opacity = 0.72, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 3, 0, 4) });
    }
    private static void AddFragment(Panel panel, string key, TextBox control, string help)
    {
        AddField(panel, key + " YAML", control, help + " Enter the value below the key, without repeating \"" + key + ":\".");
    }
    private static ScrollViewer Scroll(UIElement child)
    {
        return new ScrollViewer { Content = child, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };
    }
    private static TabItem Tab(string header, UIElement content) { return new TabItem { Header = header, Content = content }; }
    private static Brush ResourceBrush(Window owner, string key, Brush fallback)
    {
        object value = owner.TryFindResource(key);
        return value as Brush ?? fallback;
    }
}
