using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Media;
using System.Xml;

internal sealed class WindowsConfigDialog : Window
{
    private const string UnattendNamespace = "urn:schemas-microsoft-com:unattend";
    private const string WcmNamespace = "http://schemas.microsoft.com/WMIConfig/2002/State";
    private const int MaximumXmlBytes = 2 * 1024 * 1024;

    private readonly ComboBox architecture = Choices("amd64");
    private readonly ComboBox setupLanguage = LocaleChoices();
    private readonly ComboBox uiLanguage = LocaleChoices();
    private readonly ComboBox inputLocale = LocaleChoices();
    private readonly ComboBox systemLocale = LocaleChoices();
    private readonly ComboBox userLocale = LocaleChoices();
    private readonly ComboBox timeZone = new ComboBox { MinHeight = 36, MaxDropDownHeight = 260 };
    private readonly TextBox computerName = Box("");
    private readonly TextBox registeredOwner = Box(Environment.UserName);
    private readonly TextBox registeredOrganization = Box("");

    private readonly CheckBox createAccount = Check("Create a local administrator account", true);
    private readonly TextBox accountName = Box(SafeAccountName());
    private readonly TextBox displayName = Box(Environment.UserName);
    private readonly TextBox accountDescription = Box("Local administrator created by Rustfus");
    private readonly ComboBox accountGroups = Choices(
        "Administrators",
        "Administrators;Users",
        "Users",
        "Remote Desktop Users;Users");
    private readonly PasswordBox accountPassword = new PasswordBox
    {
        MinHeight = 36,
        Padding = new Thickness(8, 4, 8, 4),
        VerticalContentAlignment = VerticalAlignment.Center
    };
    private readonly CheckBox autoLogon = Check("Sign in to this account automatically", false);
    private readonly ComboBox autoLogonCount = NumberChoices(1, 20);
    private readonly CheckBox hideEula = Check("Hide the Windows license page", true);
    private readonly CheckBox hideOnline = Check("Hide Microsoft online-account pages", true);
    private readonly CheckBox hideWireless = Check("Hide wireless-network setup in OOBE", true);
    private readonly CheckBox hideLocalAccount = Check("Hide the local-account page", true);
    private readonly CheckBox hideOemRegistration = Check("Hide OEM registration", true);
    private readonly CheckBox bypassNetworkRequirement = Check("Enable offline setup when Windows requires a network", true);
    private readonly ComboBox protectYourPc = Choices("3 - do not use express settings", "1 - use recommended settings", "omit");

    private readonly CheckBox acceptEula = Check("Accept the Windows license terms", true);
    private readonly TextBox productKey = Box("");
    private readonly CheckBox disableDynamicUpdate = Check("Disable Windows Setup Dynamic Update", false);
    private readonly CheckBox bypassTpm = Check("Bypass TPM check", true);
    private readonly CheckBox bypassSecureBoot = Check("Bypass Secure Boot check", true);
    private readonly CheckBox bypassRam = Check("Bypass RAM check", true);
    private readonly CheckBox bypassCpu = Check("Bypass CPU check", false);
    private readonly CheckBox bypassStorage = Check("Bypass storage-size check", false);
    private readonly CheckBox automaticDisk = Check("Wipe and partition a destination disk automatically (UEFI/GPT)", false);
    private readonly ComboBox diskId = NumberChoices(0, 31);
    private readonly ComboBox imageIndex = OptionalNumberChoices("Let Windows Setup choose", 1, 50);

    private readonly ComboBox telemetry = Choices("Security / required only (0)", "Required (1)", "Enhanced (2)", "Full (3)", "Leave unchanged");
    private readonly CheckBox disableConsumerFeatures = Check("Disable Microsoft consumer experiences", true);
    private readonly CheckBox preventDeviceEncryption = Check("Prevent automatic device encryption", false);
    private readonly CheckBox enableLongPaths = Check("Enable Win32 long paths", false);
    private readonly CheckBox showFileExtensions = Check("Show known file extensions for the first user", true);
    private readonly CheckBox showHiddenFiles = Check("Show hidden files for the first user", false);
    private readonly TextBox windowsPeCommands = Multi("");
    private readonly TextBox specializeCommands = Multi("");
    private readonly TextBox firstLogonCommands = Multi("");

    private readonly TextBox preview = Multi("");
    private readonly TextBlock validation = new TextBlock { Margin = new Thickness(0, 8, 0, 0), TextWrapping = TextWrapping.Wrap };
    private readonly StackPanel accountOptions = Form();
    private readonly StackPanel autoLogonOptions = Form();
    private readonly StackPanel diskOptions = Form();
    private bool generationSucceeded;

    public string GeneratedXml { get; private set; }

    public WindowsConfigDialog(Window owner, string existingXml)
    {
        if (owner.IsVisible) { Owner = owner; }
        CopyThemeResources(owner);
        Title = "Windows unattended configuration";
        Width = 1040;
        Height = 780;
        MinWidth = 820;
        MinHeight = 620;
        WindowStartupLocation = owner.IsVisible
            ? WindowStartupLocation.CenterOwner
            : WindowStartupLocation.CenterScreen;
        FontFamily = owner.FontFamily;
        FontSize = owner.FontSize;
        UseLayoutRounding = true;
        SetResourceReference(BackgroundProperty, "Bg");
        SetResourceReference(ForegroundProperty, "Text");
        SourceInitialized += delegate { ApplyWindowChrome(); };

        architecture.IsEnabled = false;
        FillTimeZones();
        Grid root = new Grid { Margin = new Thickness(24, 16, 24, 20) };
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        TabControl tabs = new TabControl();
        tabs.Items.Add(Tab("Regional & identity", RegionalPanel()));
        tabs.Items.Add(Tab("Accounts & OOBE", AccountsPanel()));
        tabs.Items.Add(Tab("Setup & disk", SetupPanel()));
        tabs.Items.Add(Tab("Privacy & commands", AdvancedPanel()));
        tabs.Items.Add(Tab("Generated XML", PreviewPanel()));
        root.Children.Add(tabs);

        Grid buttons = new Grid { Margin = new Thickness(0, 14, 0, 0) };
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Button generate = new Button { Content = "Generate / refresh XML", MinWidth = 180, MinHeight = 36, Margin = new Thickness(0, 0, 8, 0) };
        Button cancel = new Button { Content = "Cancel", MinWidth = 100, MinHeight = 36, Margin = new Thickness(0, 0, 8, 0), IsCancel = true };
        Button use = new Button { Content = "Use this config", MinWidth = 140, MinHeight = 36, IsDefault = true };
        use.SetResourceReference(StyleProperty, "Primary");
        generate.Click += delegate { GenerateXml(); tabs.SelectedIndex = 4; };
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

        createAccount.Checked += delegate { UpdateEnabledState(); };
        createAccount.Unchecked += delegate { UpdateEnabledState(); };
        autoLogon.Checked += delegate { UpdateEnabledState(); };
        autoLogon.Unchecked += delegate { UpdateEnabledState(); };
        automaticDisk.Checked += delegate { UpdateEnabledState(); };
        automaticDisk.Unchecked += delegate { UpdateEnabledState(); };
        UpdateEnabledState();
        GenerateXml();

        if (!string.IsNullOrEmpty(existingXml))
        {
            preview.Text = existingXml;
            GeneratedXml = existingXml;
            validation.Text = "Loaded the current generated configuration. Change form options and regenerate it when needed.";
            tabs.SelectedIndex = 4;
        }
    }

    private UIElement RegionalPanel()
    {
        StackPanel panel = Form();
        panel.Children.Add(Heading("Architecture and language"));
        AddField(panel, "Component architecture", architecture, "Rustfus currently accepts x86-64 Windows media, so amd64 is the only valid component architecture.");
        AddField(panel, "Windows Setup UI language", setupLanguage, "Select the language pack used by Windows Setup.");
        AddField(panel, "Installed UI language", uiLanguage, "Select the default display language for the installed system.");
        AddField(panel, "Keyboard / input locale", inputLocale, "Select the Windows locale used for the default keyboard and input method.");
        AddField(panel, "System locale", systemLocale, "Select the locale used by non-Unicode programs.");
        AddField(panel, "User locale", userLocale, "Select the default formats for dates, time, numbers and currency.");
        AddField(panel, "Windows time zone", timeZone, "Uses Windows time-zone identifiers, not IANA names.");
        panel.Children.Add(Heading("Machine identity"));
        AddField(panel, "Computer name", computerName, "Leave blank to let Windows generate a name; otherwise use 1-15 letters, digits or hyphens.");
        AddField(panel, "Registered owner", registeredOwner, "Optional value stored in Windows registration information.");
        AddField(panel, "Registered organization", registeredOrganization, "Optional organization name.");
        return Scroll(panel);
    }

    private UIElement AccountsPanel()
    {
        StackPanel panel = Form();
        panel.Children.Add(Heading("Local account"));
        panel.Children.Add(createAccount);
        AddField(accountOptions, "Account name", accountName, "Local Windows account name (maximum 20 characters).");
        AddField(accountOptions, "Display name", displayName, "Friendly name shown in Windows.");
        AddField(accountOptions, "Description", accountDescription, "Optional account description.");
        AddField(accountOptions, "Local groups", accountGroups, "Select a validated set of language-neutral Windows groups.");
        AddField(accountOptions, "Password", accountPassword, "Stored as plain text in autounattend.xml on the USB. Remove the media after setup starts.");
        accountOptions.Children.Add(autoLogon);
        AddField(autoLogonOptions, "Automatic sign-in count", autoLogonCount, "Select how many automatic sign-ins Windows performs.");
        accountOptions.Children.Add(autoLogonOptions);
        panel.Children.Add(accountOptions);
        panel.Children.Add(Heading("Out-of-box experience"));
        panel.Children.Add(hideEula);
        panel.Children.Add(hideOnline);
        panel.Children.Add(hideWireless);
        panel.Children.Add(hideLocalAccount);
        panel.Children.Add(hideOemRegistration);
        panel.Children.Add(bypassNetworkRequirement);
        AddField(panel, "Privacy / protection choice", protectYourPc, "Maps to the supported OOBE ProtectYourPC setting. Rustfus does not use SkipMachineOOBE.");
        return Scroll(panel);
    }

    private UIElement SetupPanel()
    {
        StackPanel panel = Form();
        panel.Children.Add(Heading("Windows Setup"));
        panel.Children.Add(acceptEula);
        AddField(panel, "Product key", productKey, "Optional Windows product key. Leave blank to use the image defaults or enter it during setup.");
        panel.Children.Add(disableDynamicUpdate);
        panel.Children.Add(Heading("Windows 11 compatibility overrides"));
        panel.Children.Add(new TextBlock
        {
            Text = "These registry-based compatibility overrides are optional and may be changed by future Windows releases.",
            Opacity = 0.72,
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(0, 0, 0, 6)
        });
        panel.Children.Add(bypassTpm);
        panel.Children.Add(bypassSecureBoot);
        panel.Children.Add(bypassRam);
        panel.Children.Add(bypassCpu);
        panel.Children.Add(bypassStorage);
        panel.Children.Add(Heading("Destination disk"));
        panel.Children.Add(automaticDisk);
        diskOptions.Children.Add(new TextBlock
        {
            Text = "WARNING: this erases the selected disk inside the destination PC without another prompt.",
            Foreground = Brushes.OrangeRed,
            FontWeight = FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(0, 6, 0, 4)
        });
        AddField(diskOptions, "Destination disk number", diskId, "Select disk 0-31. The disk is initialized as GPT with EFI, MSR and Windows partitions.");
        AddField(diskOptions, "Install image index", imageIndex, "Select an install.wim/esd image index, or let Windows Setup choose.");
        panel.Children.Add(diskOptions);
        return Scroll(panel);
    }

    private UIElement AdvancedPanel()
    {
        StackPanel panel = Form();
        panel.Children.Add(Heading("Privacy and system defaults"));
        AddField(panel, "Diagnostic data policy", telemetry, "Writes the AllowTelemetry policy during specialize. Edition policy limits still apply.");
        panel.Children.Add(disableConsumerFeatures);
        panel.Children.Add(preventDeviceEncryption);
        panel.Children.Add(enableLongPaths);
        panel.Children.Add(showFileExtensions);
        panel.Children.Add(showHiddenFiles);
        panel.Children.Add(Heading("Commands"));
        AddCommandField(panel, "windowsPE commands", windowsPeCommands, "One command per line. Runs as SYSTEM before Windows is installed.");
        AddCommandField(panel, "specialize commands", specializeCommands, "One command per line. Runs as SYSTEM while the installed system is specialized.");
        AddCommandField(panel, "First-logon commands", firstLogonCommands, "One command per line. Runs after the first interactive sign-in.");
        panel.Children.Add(new TextBlock
        {
            Text = "Commands are free-form by nature and are inserted as XML values after character validation. Review the Generated XML tab before using the configuration.",
            Opacity = 0.72,
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(0, 6, 0, 8)
        });
        return Scroll(panel);
    }

    private UIElement PreviewPanel()
    {
        Grid panel = new Grid { Margin = new Thickness(8) };
        panel.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        panel.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        panel.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        panel.Children.Add(Heading("Complete autounattend.xml (read-only preview)"));
        preview.AcceptsReturn = true;
        preview.AcceptsTab = true;
        preview.IsReadOnly = true;
        preview.VerticalScrollBarVisibility = ScrollBarVisibility.Auto;
        preview.HorizontalScrollBarVisibility = ScrollBarVisibility.Auto;
        preview.FontFamily = new FontFamily("Consolas");
        preview.TextWrapping = TextWrapping.NoWrap;
        Grid.SetRow(preview, 1);
        panel.Children.Add(preview);
        Grid.SetRow(validation, 2);
        panel.Children.Add(validation);
        return panel;
    }

    private void UpdateEnabledState()
    {
        accountOptions.IsEnabled = createAccount.IsChecked == true;
        autoLogonOptions.IsEnabled = accountOptions.IsEnabled && autoLogon.IsChecked == true;
        diskOptions.IsEnabled = automaticDisk.IsChecked == true;
    }

    private void GenerateXml()
    {
        generationSucceeded = false;
        try
        {
            ValidateFields();
            MemoryStream stream = new MemoryStream();
            XmlWriterSettings settings = new XmlWriterSettings
            {
                Encoding = new UTF8Encoding(false),
                Indent = true,
                IndentChars = "  ",
                NewLineChars = "\r\n",
                NewLineHandling = NewLineHandling.Replace
            };
            using (XmlWriter writer = XmlWriter.Create(stream, settings))
            {
                writer.WriteStartDocument();
                writer.WriteStartElement("unattend", UnattendNamespace);
                writer.WriteAttributeString("xmlns", "wcm", null, WcmNamespace);
                WriteWindowsPe(writer);
                WriteSpecialize(writer);
                WriteOobe(writer);
                writer.WriteEndElement();
                writer.WriteEndDocument();
            }
            string xml = Encoding.UTF8.GetString(stream.ToArray());
            ValidateXml(xml);
            preview.Text = xml;
            generationSucceeded = true;
            GeneratedXml = xml;
            validation.Text = "Valid Microsoft unattended-setup document. Generated locally by Rustfus.";
        }
        catch (Exception ex)
        {
            validation.Text = "Cannot generate XML: " + ex.Message;
        }
    }

    private void WriteWindowsPe(XmlWriter writer)
    {
        Start(writer, "settings");
        writer.WriteAttributeString("pass", "windowsPE");
        StartComponent(writer, "Microsoft-Windows-International-Core-WinPE");
        Element(writer, "InputLocale", SelectedValue(inputLocale));
        Element(writer, "SystemLocale", SelectedValue(systemLocale));
        Element(writer, "UILanguage", SelectedValue(setupLanguage));
        Element(writer, "UserLocale", SelectedValue(userLocale));
        Start(writer, "SetupUILanguage");
        Element(writer, "UILanguage", SelectedValue(setupLanguage));
        writer.WriteEndElement();
        writer.WriteEndElement();

        StartComponent(writer, "Microsoft-Windows-Setup");
        int order = 1;
        List<string> commands = new List<string>();
        AddBypass(commands, bypassTpm, "BypassTPMCheck");
        AddBypass(commands, bypassSecureBoot, "BypassSecureBootCheck");
        AddBypass(commands, bypassRam, "BypassRAMCheck");
        AddBypass(commands, bypassCpu, "BypassCPUCheck");
        AddBypass(commands, bypassStorage, "BypassStorageCheck");
        commands.AddRange(Lines(windowsPeCommands.Text));
        WriteRunSynchronous(writer, commands, ref order);
        if (automaticDisk.IsChecked == true) { WriteDiskConfiguration(writer); }
        if (disableDynamicUpdate.IsChecked == true)
        {
            Start(writer, "DynamicUpdate");
            Element(writer, "Enable", "false");
            writer.WriteEndElement();
        }
        if (acceptEula.IsChecked == true || productKey.Text.Trim().Length > 0 ||
            registeredOwner.Text.Trim().Length > 0 || registeredOrganization.Text.Trim().Length > 0)
        {
            Start(writer, "UserData");
            if (acceptEula.IsChecked == true) { Element(writer, "AcceptEula", "true"); }
            OptionalElement(writer, "FullName", registeredOwner.Text);
            OptionalElement(writer, "Organization", registeredOrganization.Text);
            if (productKey.Text.Trim().Length > 0)
            {
                Start(writer, "ProductKey");
                Element(writer, "Key", productKey.Text.Trim());
                Element(writer, "WillShowUI", "OnError");
                writer.WriteEndElement();
            }
            writer.WriteEndElement();
        }
        writer.WriteEndElement();
        writer.WriteEndElement();
    }

    private void WriteDiskConfiguration(XmlWriter writer)
    {
        string selectedDisk = SelectedValue(diskId);
        Start(writer, "DiskConfiguration");
        Start(writer, "Disk");
        ActionAdd(writer);
        Element(writer, "DiskID", selectedDisk);
        Element(writer, "WillWipeDisk", "true");
        Start(writer, "CreatePartitions");
        WriteCreatePartition(writer, 1, "EFI", "100", false);
        WriteCreatePartition(writer, 2, "MSR", "16", false);
        WriteCreatePartition(writer, 3, "Primary", null, true);
        writer.WriteEndElement();
        Start(writer, "ModifyPartitions");
        WriteModifyPartition(writer, 1, 1, "FAT32", "System", null);
        WriteModifyPartition(writer, 2, 3, "NTFS", "Windows", "C");
        writer.WriteEndElement();
        writer.WriteEndElement();
        Element(writer, "WillShowUI", "OnError");
        writer.WriteEndElement();

        Start(writer, "ImageInstall");
        Start(writer, "OSImage");
        string selectedImage = SelectedValue(imageIndex);
        if (selectedImage.Length > 0)
        {
            Start(writer, "InstallFrom");
            Start(writer, "MetaData");
            ActionAdd(writer);
            Element(writer, "Key", "/IMAGE/INDEX");
            Element(writer, "Value", selectedImage);
            writer.WriteEndElement();
            writer.WriteEndElement();
        }
        Start(writer, "InstallTo");
        Element(writer, "DiskID", selectedDisk);
        Element(writer, "PartitionID", "3");
        writer.WriteEndElement();
        Element(writer, "WillShowUI", "OnError");
        writer.WriteEndElement();
        writer.WriteEndElement();
    }

    private void WriteSpecialize(XmlWriter writer)
    {
        Start(writer, "settings");
        writer.WriteAttributeString("pass", "specialize");
        StartComponent(writer, "Microsoft-Windows-Shell-Setup");
        OptionalElement(writer, "ComputerName", computerName.Text);
        Element(writer, "TimeZone", SelectedValue(timeZone));
        OptionalElement(writer, "RegisteredOwner", registeredOwner.Text);
        OptionalElement(writer, "RegisteredOrganization", registeredOrganization.Text);
        writer.WriteEndElement();

        List<string> commands = new List<string>();
        if (bypassNetworkRequirement.IsChecked == true)
        {
            commands.Add("reg.exe add \"HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\OOBE\" /v BypassNRO /t REG_DWORD /d 1 /f");
        }
        string telemetryValue = TelemetryValue();
        if (telemetryValue != null)
        {
            commands.Add("reg.exe add \"HKLM\\SOFTWARE\\Policies\\Microsoft\\Windows\\DataCollection\" /v AllowTelemetry /t REG_DWORD /d " + telemetryValue + " /f");
        }
        if (disableConsumerFeatures.IsChecked == true)
        {
            commands.Add("reg.exe add \"HKLM\\SOFTWARE\\Policies\\Microsoft\\Windows\\CloudContent\" /v DisableWindowsConsumerFeatures /t REG_DWORD /d 1 /f");
        }
        if (preventDeviceEncryption.IsChecked == true)
        {
            commands.Add("reg.exe add \"HKLM\\SYSTEM\\CurrentControlSet\\Control\\BitLocker\" /v PreventDeviceEncryption /t REG_DWORD /d 1 /f");
        }
        if (enableLongPaths.IsChecked == true)
        {
            commands.Add("reg.exe add \"HKLM\\SYSTEM\\CurrentControlSet\\Control\\FileSystem\" /v LongPathsEnabled /t REG_DWORD /d 1 /f");
        }
        commands.AddRange(Lines(specializeCommands.Text));
        if (commands.Count > 0)
        {
            StartComponent(writer, "Microsoft-Windows-Deployment");
            int order = 1;
            WriteRunSynchronous(writer, commands, ref order);
            writer.WriteEndElement();
        }
        writer.WriteEndElement();
    }

    private void WriteOobe(XmlWriter writer)
    {
        Start(writer, "settings");
        writer.WriteAttributeString("pass", "oobeSystem");
        StartComponent(writer, "Microsoft-Windows-International-Core");
        Element(writer, "InputLocale", SelectedValue(inputLocale));
        Element(writer, "SystemLocale", SelectedValue(systemLocale));
        Element(writer, "UILanguage", SelectedValue(uiLanguage));
        Element(writer, "UserLocale", SelectedValue(userLocale));
        writer.WriteEndElement();

        StartComponent(writer, "Microsoft-Windows-Shell-Setup");
        if (createAccount.IsChecked == true) { WriteAccount(writer); }
        Start(writer, "OOBE");
        Element(writer, "HideEULAPage", Bool(hideEula));
        Element(writer, "HideOnlineAccountScreens", Bool(hideOnline));
        Element(writer, "HideWirelessSetupInOOBE", Bool(hideWireless));
        Element(writer, "HideLocalAccountScreen", Bool(hideLocalAccount));
        Element(writer, "HideOEMRegistrationScreen", Bool(hideOemRegistration));
        if (protectYourPc.SelectedIndex != 2)
        {
            Element(writer, "ProtectYourPC", protectYourPc.SelectedIndex == 0 ? "3" : "1");
        }
        writer.WriteEndElement();

        List<string> firstCommands = new List<string>();
        if (showFileExtensions.IsChecked == true)
        {
            firstCommands.Add("reg.exe add \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced\" /v HideFileExt /t REG_DWORD /d 0 /f");
        }
        if (showHiddenFiles.IsChecked == true)
        {
            firstCommands.Add("reg.exe add \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced\" /v Hidden /t REG_DWORD /d 1 /f");
        }
        firstCommands.AddRange(Lines(firstLogonCommands.Text));
        WriteFirstLogonCommands(writer, firstCommands);
        writer.WriteEndElement();
        writer.WriteEndElement();
    }

    private void WriteAccount(XmlWriter writer)
    {
        string name = accountName.Text.Trim();
        string password = accountPassword.Password;
        Start(writer, "UserAccounts");
        Start(writer, "LocalAccounts");
        Start(writer, "LocalAccount");
        ActionAdd(writer);
        Start(writer, "Password");
        Element(writer, "Value", password);
        Element(writer, "PlainText", "true");
        writer.WriteEndElement();
        OptionalElement(writer, "Description", accountDescription.Text);
        OptionalElement(writer, "DisplayName", displayName.Text);
        Element(writer, "Group", SelectedValue(accountGroups));
        Element(writer, "Name", name);
        writer.WriteEndElement();
        writer.WriteEndElement();
        writer.WriteEndElement();
        if (autoLogon.IsChecked == true)
        {
            Start(writer, "AutoLogon");
            Start(writer, "Password");
            Element(writer, "Value", password);
            Element(writer, "PlainText", "true");
            writer.WriteEndElement();
            Element(writer, "Enabled", "true");
            Element(writer, "LogonCount", SelectedValue(autoLogonCount));
            Element(writer, "Username", name);
            writer.WriteEndElement();
        }
    }

    private void Accept()
    {
        try
        {
            if (!generationSucceeded) { GenerateXml(); }
            ValidateXml(preview.Text);
            GeneratedXml = preview.Text;
            DialogResult = true;
        }
        catch (Exception ex)
        {
            validation.Text = "Cannot use this XML: " + ex.Message;
        }
    }

    public static void ValidateXml(string xml)
    {
        if (string.IsNullOrWhiteSpace(xml))
        {
            throw new InvalidOperationException("autounattend.xml is empty.");
        }
        if (Encoding.UTF8.GetByteCount(xml) > MaximumXmlBytes)
        {
            throw new InvalidOperationException("autounattend.xml is larger than 2 MiB.");
        }
        if (xml.IndexOf("<!DOCTYPE", StringComparison.OrdinalIgnoreCase) >= 0)
        {
            throw new InvalidOperationException("DOCTYPE declarations are not allowed.");
        }
        XmlDocument document = new XmlDocument();
        document.PreserveWhitespace = true;
        document.XmlResolver = null;
        try { document.LoadXml(xml); }
        catch (XmlException ex)
        {
            throw new InvalidOperationException("Malformed XML: " + ex.Message);
        }
        if (document.DocumentElement == null || document.DocumentElement.LocalName != "unattend" ||
            document.DocumentElement.NamespaceURI != UnattendNamespace)
        {
            throw new InvalidOperationException("The root must be a Microsoft unattend document.");
        }
    }

    private void ValidateFields()
    {
        Require(SelectedValue(setupLanguage), "Windows Setup UI language");
        Require(SelectedValue(uiLanguage), "Installed UI language");
        Require(SelectedValue(inputLocale), "Input locale");
        Require(SelectedValue(systemLocale), "System locale");
        Require(SelectedValue(userLocale), "User locale");
        Require(SelectedValue(timeZone), "Time zone");
        if (!ValidComputerName(computerName.Text.Trim()))
        {
            throw new InvalidOperationException("Computer name must be blank or 1-15 letters, digits or hyphens, and cannot be all digits.");
        }
        ValidateFreeText(registeredOwner.Text, "Registered owner", 100);
        ValidateFreeText(registeredOrganization.Text, "Registered organization", 100);
        if (productKey.Text.Trim().Length > 0 && !ValidProductKey(productKey.Text.Trim()))
        {
            throw new InvalidOperationException("Product key must use the XXXXX-XXXXX-XXXXX-XXXXX-XXXXX format.");
        }
        if (createAccount.IsChecked == true)
        {
            if (!ValidAccountName(accountName.Text.Trim()))
            {
                throw new InvalidOperationException("Enter a valid local account name (1-20 characters and not a Windows reserved name).");
            }
            Require(SelectedValue(accountGroups), "Local groups");
            ValidateFreeText(displayName.Text, "Display name", 256);
            ValidateFreeText(accountDescription.Text, "Account description", 256);
            if (accountPassword.Password.Length > 127 || ContainsControl(accountPassword.Password))
            {
                throw new InvalidOperationException("The account password must be at most 127 characters and contain no control characters.");
            }
            if (autoLogon.IsChecked == true) { Require(SelectedValue(autoLogonCount), "Automatic sign-in count"); }
        }
        if (automaticDisk.IsChecked == true)
        {
            Require(SelectedValue(diskId), "Destination disk number");
        }
        if (ContainsControl(productKey.Text) || ContainsControlExceptLines(windowsPeCommands.Text) ||
            ContainsControlExceptLines(specializeCommands.Text) || ContainsControlExceptLines(firstLogonCommands.Text))
        {
            throw new InvalidOperationException("A product key or command contains unsupported control characters.");
        }
    }

    private static void WriteRunSynchronous(XmlWriter writer, List<string> commands, ref int order)
    {
        if (commands.Count == 0) { return; }
        Start(writer, "RunSynchronous");
        foreach (string command in commands)
        {
            Start(writer, "RunSynchronousCommand");
            ActionAdd(writer);
            Element(writer, "Order", order.ToString(CultureInfo.InvariantCulture));
            Element(writer, "Path", command);
            writer.WriteEndElement();
            order++;
        }
        writer.WriteEndElement();
    }

    private static void WriteFirstLogonCommands(XmlWriter writer, List<string> commands)
    {
        if (commands.Count == 0) { return; }
        Start(writer, "FirstLogonCommands");
        int order = 1;
        foreach (string command in commands)
        {
            Start(writer, "SynchronousCommand");
            ActionAdd(writer);
            Element(writer, "Order", order.ToString(CultureInfo.InvariantCulture));
            Element(writer, "CommandLine", command);
            Element(writer, "Description", "Rustfus command " + order.ToString(CultureInfo.InvariantCulture));
            writer.WriteEndElement();
            order++;
        }
        writer.WriteEndElement();
    }

    private static void WriteCreatePartition(XmlWriter writer, int order, string type, string size, bool extend)
    {
        Start(writer, "CreatePartition");
        ActionAdd(writer);
        Element(writer, "Order", order.ToString(CultureInfo.InvariantCulture));
        Element(writer, "Type", type);
        if (size != null) { Element(writer, "Size", size); }
        if (extend) { Element(writer, "Extend", "true"); }
        writer.WriteEndElement();
    }

    private static void WriteModifyPartition(XmlWriter writer, int order, int partition, string format, string label, string letter)
    {
        Start(writer, "ModifyPartition");
        ActionAdd(writer);
        Element(writer, "Order", order.ToString(CultureInfo.InvariantCulture));
        Element(writer, "PartitionID", partition.ToString(CultureInfo.InvariantCulture));
        Element(writer, "Format", format);
        Element(writer, "Label", label);
        if (letter != null) { Element(writer, "Letter", letter); }
        writer.WriteEndElement();
    }

    private static void AddBypass(List<string> commands, CheckBox enabled, string value)
    {
        if (enabled.IsChecked == true)
        {
            commands.Add("reg.exe add \"HKLM\\SYSTEM\\Setup\\LabConfig\" /v " + value + " /t REG_DWORD /d 1 /f");
        }
    }

    private string TelemetryValue()
    {
        return telemetry.SelectedIndex >= 0 && telemetry.SelectedIndex <= 3
            ? telemetry.SelectedIndex.ToString(CultureInfo.InvariantCulture)
            : null;
    }

    private void StartComponent(XmlWriter writer, string name)
    {
        Start(writer, "component");
        writer.WriteAttributeString("name", name);
        writer.WriteAttributeString("processorArchitecture", SelectedValue(architecture));
        writer.WriteAttributeString("publicKeyToken", "31bf3856ad364e35");
        writer.WriteAttributeString("language", "neutral");
        writer.WriteAttributeString("versionScope", "nonSxS");
    }

    private static void Start(XmlWriter writer, string name) { writer.WriteStartElement(name, UnattendNamespace); }
    private static void Element(XmlWriter writer, string name, string value)
    {
        writer.WriteElementString(name, UnattendNamespace, value);
    }
    private static void OptionalElement(XmlWriter writer, string name, string value)
    {
        if (!string.IsNullOrWhiteSpace(value)) { Element(writer, name, value.Trim()); }
    }
    private static void ActionAdd(XmlWriter writer) { writer.WriteAttributeString("wcm", "action", WcmNamespace, "add"); }
    private static string Bool(CheckBox box) { return box.IsChecked == true ? "true" : "false"; }

    private static string SelectedValue(ComboBox box)
    {
        ComboBoxItem item = box.SelectedItem as ComboBoxItem;
        if (item != null)
        {
            return item.Tag == null
                ? Convert.ToString(item.Content, CultureInfo.InvariantCulture)
                : Convert.ToString(item.Tag, CultureInfo.InvariantCulture);
        }
        return box.SelectedItem == null
            ? ""
            : Convert.ToString(box.SelectedItem, CultureInfo.InvariantCulture);
    }

    private static void Require(string value, string label)
    {
        if (string.IsNullOrWhiteSpace(value)) { throw new InvalidOperationException(label + " is required."); }
    }

    private static void ValidateFreeText(string value, string label, int maximumLength)
    {
        if (value.Length > maximumLength || ContainsControl(value))
        {
            throw new InvalidOperationException(label + " must be at most " +
                maximumLength.ToString(CultureInfo.InvariantCulture) + " characters and contain no control characters.");
        }
    }

    private static bool ValidProductKey(string value)
    {
        if (value.Length != 29) { return false; }
        for (int index = 0; index < value.Length; index++)
        {
            if ((index + 1) % 6 == 0)
            {
                if (value[index] != '-') { return false; }
            }
            else if (!IsAsciiLetterOrDigit(value[index])) { return false; }
        }
        return true;
    }

    private static bool ValidAccountName(string name)
    {
        if (string.IsNullOrWhiteSpace(name) || name != name.Trim() || name.Length > 20 || name.EndsWith(".", StringComparison.Ordinal)) { return false; }
        if (name.IndexOfAny(new[] { '/', '\\', '[', ']', ':', ';', '|', '=', ',', '+', '*', '?', '<', '>', '"', '%' }) >= 0) { return false; }
        string[] reserved = { "administrator", "guest", "defaultaccount", "system", "network service", "local service", "none", "wdagutilityaccount" };
        foreach (string value in reserved)
        {
            if (string.Equals(name, value, StringComparison.OrdinalIgnoreCase)) { return false; }
        }
        return !ContainsControl(name);
    }

    private static bool ValidComputerName(string name)
    {
        if (name.Length == 0) { return true; }
        if (name.Length > 15 || name[0] == '-' || name[name.Length - 1] == '-') { return false; }
        bool allDigits = true;
        foreach (char character in name)
        {
            if (!IsAsciiLetterOrDigit(character) && character != '-') { return false; }
            if (character < '0' || character > '9') { allDigits = false; }
        }
        return !allDigits;
    }

    private static bool IsAsciiLetterOrDigit(char value)
    {
        return (value >= 'A' && value <= 'Z') || (value >= 'a' && value <= 'z') ||
            (value >= '0' && value <= '9');
    }

    private static bool ContainsControl(string text)
    {
        foreach (char character in text) { if (char.IsControl(character)) { return true; } }
        return false;
    }

    private static bool ContainsControlExceptLines(string text)
    {
        foreach (char character in text)
        {
            if (char.IsControl(character) && character != '\r' && character != '\n' && character != '\t') { return true; }
        }
        return false;
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

    private void FillTimeZones()
    {
        string current = TimeZoneInfo.Local.Id;
        foreach (TimeZoneInfo zone in TimeZoneInfo.GetSystemTimeZones())
        {
            timeZone.Items.Add(zone.Id);
            if (string.Equals(zone.Id, current, StringComparison.OrdinalIgnoreCase)) { timeZone.SelectedItem = zone.Id; }
        }
        if (timeZone.SelectedIndex < 0 && timeZone.Items.Count > 0) { timeZone.SelectedIndex = 0; }
    }

    private static ComboBox LocaleChoices()
    {
        ComboBox box = new ComboBox { MinHeight = 36, MaxDropDownHeight = 260 };
        string current = CurrentLocale();
        List<CultureInfo> cultures = new List<CultureInfo>();
        foreach (CultureInfo culture in CultureInfo.GetCultures(CultureTypes.SpecificCultures))
        {
            if (!string.IsNullOrEmpty(culture.Name)) { cultures.Add(culture); }
        }
        cultures.Sort(delegate(CultureInfo left, CultureInfo right)
        {
            return StringComparer.CurrentCultureIgnoreCase.Compare(left.DisplayName, right.DisplayName);
        });
        foreach (CultureInfo culture in cultures)
        {
            ComboBoxItem item = new ComboBoxItem
            {
                Content = culture.DisplayName + " (" + culture.Name + ")",
                Tag = culture.Name
            };
            box.Items.Add(item);
            if (string.Equals(culture.Name, current, StringComparison.OrdinalIgnoreCase)) { box.SelectedItem = item; }
        }
        if (box.SelectedIndex < 0 && box.Items.Count > 0) { box.SelectedIndex = 0; }
        return box;
    }

    private static string CurrentLocale()
    {
        string name = CultureInfo.CurrentCulture.Name;
        return string.IsNullOrEmpty(name) ? "en-US" : name;
    }

    private static string SafeAccountName()
    {
        string value = Environment.UserName == null ? "User" : Environment.UserName.Trim();
        return ValidAccountName(value) ? value : "User";
    }

    private static StackPanel Form() { return new StackPanel { Margin = new Thickness(8) }; }
    private static TextBox Box(string text)
    {
        return new TextBox
        {
            Text = text,
            MinHeight = 36,
            Padding = new Thickness(8, 4, 8, 4),
            VerticalContentAlignment = VerticalAlignment.Center
        };
    }
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
        ComboBox box = new ComboBox { MinHeight = 36 };
        foreach (string value in values) { box.Items.Add(value); }
        box.SelectedIndex = 0;
        return box;
    }
    private static ComboBox NumberChoices(int first, int last)
    {
        ComboBox box = new ComboBox { MinHeight = 36, MaxDropDownHeight = 260 };
        for (int value = first; value <= last; value++)
        {
            box.Items.Add(value.ToString(CultureInfo.InvariantCulture));
        }
        box.SelectedIndex = 0;
        return box;
    }
    private static ComboBox OptionalNumberChoices(string emptyLabel, int first, int last)
    {
        ComboBox box = NumberChoices(first, last);
        ComboBoxItem empty = new ComboBoxItem { Content = emptyLabel, Tag = "" };
        box.Items.Insert(0, empty);
        box.SelectedIndex = 0;
        return box;
    }
    private static TextBlock Heading(string text)
    {
        return new TextBlock { Text = text, FontSize = 14, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 12, 0, 8) };
    }
    private static void AddField(Panel panel, string label, Control control, string help)
    {
        panel.Children.Add(new TextBlock { Text = label, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 8, 0, 4) });
        panel.Children.Add(control);
        panel.Children.Add(new TextBlock { Text = help, Opacity = 0.72, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 3, 0, 4) });
    }
    private static void AddCommandField(Panel panel, string label, TextBox control, string help)
    {
        AddField(panel, label, control, help);
    }
    private static ScrollViewer Scroll(UIElement child)
    {
        return new ScrollViewer { Content = child, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };
    }
    private static TabItem Tab(string header, UIElement content) { return new TabItem { Header = header, Content = content }; }

    private void ApplyWindowChrome()
    {
        SolidColorBrush background = TryFindResource("Bg") as SolidColorBrush;
        bool darkTheme = background != null && (background.Color.R + background.Color.G + background.Color.B) < 384;
        int enabled = darkTheme ? 1 : 0;
        IntPtr handle = new WindowInteropHelper(this).Handle;
        if (handle != IntPtr.Zero)
        {
            Native.DwmSetWindowAttribute(handle, 20, ref enabled, 4);
            Native.DwmSetWindowAttribute(handle, 19, ref enabled, 4);
        }
    }

    private void CopyThemeResources(Window owner)
    {
        foreach (object key in owner.Resources.Keys) { Resources[key] = owner.Resources[key]; }
    }
}
