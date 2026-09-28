using System;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Threading.Tasks;
using Avalonia.Controls;
using Avalonia.Input.Platform;
using Avalonia.Interactivity;

namespace SteamCurator.Gui;

public partial class MainWindow : Window
{
    private readonly CuratorCli _cli = new();
    private bool _busy;
    private bool _confirmWrite;

    public MainWindow()
    {
        InitializeComponent();

        var boot = new StringBuilder();
        boot.AppendLine("steam-curator　前端：Avalonia　·　后端：Rust CLI");
        boot.AppendLine(new string('─', 56));
        boot.AppendLine();
        boot.AppendLine("使用顺序：");
        boot.AppendLine("  ① 扫描 Steam 库");
        boot.AppendLine("  ② 生成 AI Prompt → 复制到剪贴板 → 贴给任意 AI");
        boot.AppendLine("  ③ 把 AI 的回复整段粘到下方输入框，点「校验 AI 回复」");
        boot.AppendLine("  ④ 打开预览报告确认效果");
        boot.AppendLine("  ⑤ 演练写回（不改文件）→ ⑥ 正式写回");
        boot.AppendLine();
        boot.AppendLine("输出目录＝仓库根下的 output/，点左下「打开输出目录」直接打开它。");
        LogBox.Text = boot.ToString();

        RefreshStatus();
    }

    // ── 状态栏 ────────────────────────────────────────────────

    private void RefreshStatus()
    {
        StatusPlan.Text = File.Exists(_cli.PlanPath) ? "已生成整理方案" : "尚无整理方案";

        var steamRunning = Process.GetProcessesByName("steam").Length > 0;
        StatusSteam.Text = steamRunning
            ? "Steam 正在运行 —— 写入会被拒绝"
            : "Steam 未运行，可安全写入";
    }

    // ── 通用执行 ──────────────────────────────────────────────

    private async Task RunAsync(Func<Task<string>> action, string label)
    {
        if (_busy)
        {
            return;
        }

        _busy = true;
        SetEnabled(false);
        Show($"{label}…\n");

        try
        {
            var output = await action();
            Show(output.Length > 0 ? output : "（无输出）");
        }
        catch (Exception ex)
        {
            Show($"✖ 出错：{ex.Message}");
        }
        finally
        {
            _busy = false;
            SetEnabled(true);
            RefreshStatus();
        }
    }

    private void Show(string text) => LogBox.Text = text;

    private void SetEnabled(bool enabled)
    {
        BtnScan.IsEnabled = enabled;
        BtnPrompt.IsEnabled = enabled;
        BtnCopy.IsEnabled = enabled;
        BtnPlan.IsEnabled = enabled;
        BtnPreview.IsEnabled = enabled;
        BtnDry.IsEnabled = enabled;
        BtnWrite.IsEnabled = enabled;
        BtnRestore.IsEnabled = enabled;
        BtnInspect.IsEnabled = enabled;
        Busy.IsVisible = !enabled;
    }

    // ── ① 准备 ────────────────────────────────────────────────

    private async void OnScan(object? sender, RoutedEventArgs e) =>
        await RunAsync(() => _cli.ScanAsync(), "正在扫描 Steam 库");

    private async void OnPrompt(object? sender, RoutedEventArgs e) =>
        await RunAsync(() => _cli.PromptAsync(), "正在生成 AI Prompt");

    private async void OnCopyPrompt(object? sender, RoutedEventArgs e)
    {
        if (!File.Exists(_cli.PromptPath))
        {
            Show($"⚠ 还没生成 prompt。请先点「生成 AI Prompt」。\n\n（找不到 {_cli.PromptPath}）");
            return;
        }

        var text = await File.ReadAllTextAsync(_cli.PromptPath, Encoding.UTF8);
        var clipboard = GetTopLevel(this)?.Clipboard;
        if (clipboard is null)
        {
            Show("⚠ 拿不到剪贴板，请改用「打开输出目录」手动打开 AI-PROMPT.md。");
            return;
        }

        await clipboard.SetTextAsync(text);
        Show($"✔ 已把 AI Prompt（{text.Length} 字符）复制到剪贴板。\n\n现在粘贴给任意对话式 AI 即可。");
    }

    // ── ② 整理 ────────────────────────────────────────────────

    private async void OnPlan(object? sender, RoutedEventArgs e)
    {
        var reply = ReplyBox.Text ?? string.Empty;
        if (reply.Trim().Length == 0)
        {
            Show("输入框是空的。\n\n请把 AI 的回复整段粘到右下角的框里再点「校验 AI 回复」。");
            return;
        }

        await RunAsync(() => _cli.PlanFromReplyAsync(reply), "正在校验 AI 回复");
    }

    private async void OnPreview(object? sender, RoutedEventArgs e)
    {
        await RunAsync(async () =>
        {
            var output = await _cli.PreviewAsync();
            if (File.Exists(_cli.ReportPath))
            {
                CuratorCli.OpenWithShell(_cli.ReportPath);
            }
            return output;
        }, "正在生成预览报告");
    }

    // ── ③ 写回 ────────────────────────────────────────────────

    private async void OnDryRun(object? sender, RoutedEventArgs e) =>
        await RunAsync(() => _cli.ApplyAsync(write: false), "正在演练写回");

    private async void OnWrite(object? sender, RoutedEventArgs e)
    {
        if (!_confirmWrite)
        {
            _confirmWrite = true;
            BtnWrite.Content = "⚠ 再点一次确认";
            Show(
                "【确认】正式写回会修改你 Steam 里的合集。\n\n" +
                "写之前请确认：\n" +
                "  1. Steam 已完全退出（包括右下角托盘图标）；\n" +
                "  2. 你已经用「演练写回」看过结果，确认无误。\n\n" +
                "写之前程序会自动把合集文件、命名空间版本文件、localconfig.vdf\n" +
                "整份备份到 output\\backups\\ 下，随时可用「回滚最近备份」还原。\n\n" +
                "确认无误就再点一次「⚠ 再点一次确认」。");
            return;
        }

        _confirmWrite = false;
        BtnWrite.Content = "正式写回";
        await RunAsync(() => _cli.ApplyAsync(write: true), "正在写回 Steam 合集");
    }

    private async void OnRestore(object? sender, RoutedEventArgs e) =>
        await RunAsync(() => _cli.RestoreAsync(), "正在回滚最近备份");

    // ── ④ 其它 ────────────────────────────────────────────────

    private async void OnInspect(object? sender, RoutedEventArgs e) =>
        await RunAsync(() => _cli.InspectAsync(), "正在读取库现状");

    private void OnOpenOutDir(object? sender, RoutedEventArgs e)
    {
        Directory.CreateDirectory(_cli.OutDir);
        CuratorCli.OpenWithShell(_cli.OutDir);
    }

    private void OnClear(object? sender, RoutedEventArgs e) => Show(string.Empty);
}
