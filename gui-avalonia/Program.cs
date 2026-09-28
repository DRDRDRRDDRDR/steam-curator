using System;
using Avalonia;

namespace SteamCurator.Gui;

internal static class Program
{
    // Avalonia 要求 UI 线程是 STA
    [STAThread]
    public static void Main(string[] args) =>
        BuildAvaloniaApp().StartWithClassicDesktopLifetime(args);

    public static AppBuilder BuildAvaloniaApp() =>
        AppBuilder.Configure<App>()
            .UsePlatformDetect()
            // Inter 提供现代感的拉丁字形；中文由字体回退到系统字体（见 App.axaml 的 FontFamily 列表）
            .WithInterFont()
            .LogToTrace();
}
