use super::run_shell;

#[test]
fn worker_powershell_runs_without_a_visible_console_and_returns_output() {
    let script = r#"Add-Type -Name Window -Namespace Probe -MemberDefinition '
        [DllImport("kernel32.dll")] public static extern System.IntPtr GetConsoleWindow();
        [DllImport("user32.dll")] public static extern bool IsWindowVisible(System.IntPtr hwnd);';
        $console = [Probe.Window]::GetConsoleWindow();
        Write-Output ("worker-console-visible=" + [Probe.Window]::IsWindowVisible($console))"#;
    let output = run_shell(std::path::Path::new("."), script, &[]);
    assert!(output.contains("worker-console-visible=False"), "{output}");
}
