using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Threading.Tasks;

namespace SteamCurator.Gui;

/// <summary>
/// 后端包装：所有真正的逻辑都在 Rust 那份 CLI 里（43 个测试覆盖），
/// 前端只负责拼参数、跑进程、把输出回显。这样两边不会行为分叉。
/// </summary>
public sealed class CuratorCli
{
    public string CliPath { get; }
    public string OutDir { get; }

    public CuratorCli() => (CliPath, OutDir) = Resolve();

    /// <summary>
    /// 找 CLI 与输出目录。
    ///
    /// 从当前程序集所在目录向上找 <c>Cargo.toml</c>：命中就用
    /// <c>&lt;仓库&gt;/target/release|debug/steam-curator.exe</c> 与 <c>&lt;仓库&gt;/output</c>。
    /// 这样在 IDE 里跑、或双击发布后的 exe，看到的都是同一份数据。
    /// </summary>
    private static (string cli, string outDir) Resolve()
    {
        var baseDir = AppContext.BaseDirectory;
        string? repoRoot = null;

        for (var dir = new DirectoryInfo(baseDir); dir is not null; dir = dir.Parent)
        {
            if (File.Exists(Path.Combine(dir.FullName, "Cargo.toml")))
            {
                repoRoot = dir.FullName;
                break;
            }
        }

        var candidates = new List<string> { Path.Combine(baseDir, "steam-curator.exe") };
        if (repoRoot is not null)
        {
            candidates.Add(Path.Combine(repoRoot, "target", "release", "steam-curator.exe"));
            candidates.Add(Path.Combine(repoRoot, "target", "debug", "steam-curator.exe"));
        }

        var cli = candidates.Find(File.Exists) ?? candidates[^1];

        var outDir = repoRoot is not null
            ? Path.Combine(repoRoot, "output")
            : Path.Combine(baseDir, "output");

        return (cli, outDir);
    }

    /// <summary>跑一次 CLI。带 <paramref name="stdin"/> 时走标准输入（用于粘贴 AI 回复）。</summary>
    public async Task<string> RunAsync(string arguments, string? stdin = null)
    {
        if (!File.Exists(CliPath))
        {
            return $"✖ 找不到后端程序：{CliPath}\n\n请先在仓库根目录执行 `cargo build --release`。";
        }

        var psi = new ProcessStartInfo(CliPath, $"{arguments} --out \"{OutDir}\"")
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            RedirectStandardInput = stdin is not null,
            UseShellExecute = false,
            CreateNoWindow = true,
            StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8,
            WorkingDirectory = Path.GetDirectoryName(CliPath)!,
        };

        using var proc = Process.Start(psi);
        if (proc is null)
        {
            return $"✖ 无法启动后端程序：{CliPath}";
        }

        var stdoutTask = proc.StandardOutput.ReadToEndAsync();
        var stderrTask = proc.StandardError.ReadToEndAsync();

        if (stdin is not null)
        {
            await proc.StandardInput.WriteAsync(stdin);
            proc.StandardInput.Close();
        }

        await proc.WaitForExitAsync();
        var text = new StringBuilder();
        text.Append(await stdoutTask);
        var err = await stderrTask;
        if (!string.IsNullOrWhiteSpace(err))
        {
            text.Append('\n').Append(err);
        }

        return text.ToString().TrimEnd();
    }

    public Task<string> ScanAsync(bool installedOnly = false) =>
        RunAsync(installedOnly ? "scan --installed-only" : "scan");

    public Task<string> PromptAsync(int maxGames = 0) =>
        RunAsync(maxGames > 0 ? $"prompt --max-games {maxGames}" : "prompt");

    public Task<string> PlanFromReplyAsync(string reply) => RunAsync("plan -i -", reply);

    public Task<string> PreviewAsync() => RunAsync("preview");

    public Task<string> ApplyAsync(bool write) => RunAsync(write ? "apply --write" : "apply");

    public Task<string> RestoreAsync() => RunAsync("restore --latest --confirm");

    public Task<string> InspectAsync() => RunAsync("inspect");

    public string ReportPath => Path.Combine(OutDir, "report.html");

    public string PromptPath => Path.Combine(OutDir, "AI-PROMPT.md");

    public string PlanPath => Path.Combine(OutDir, "plan.json");

    public string LibraryPath => Path.Combine(OutDir, "library.json");

    public static void OpenWithShell(string path)
    {
        try
        {
            Process.Start(new ProcessStartInfo(path) { UseShellExecute = true });
        }
        catch
        {
            // 打不开就算了，不打断主流程
        }
    }
}
