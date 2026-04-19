# Lite Anvil file association installer for Windows.
# Run as Administrator: powershell -ExecutionPolicy Bypass -File install-file-associations.ps1
#
# Registers Lite Anvil as an "Open With" option for supported file types.
# Does NOT change default associations — only adds to the Open With list.

param(
    [string]$ExePath = (Join-Path $PSScriptRoot "..\..\lite-anvil.exe")
)

$ExePath = (Resolve-Path $ExePath -ErrorAction Stop).Path
$AppId = "LiteAnvil.Editor"

Write-Host "Registering Lite Anvil file associations..."
Write-Host "Executable: $ExePath"

# Register the application class
$classKey = "HKCU:\Software\Classes\$AppId"
New-Item -Path $classKey -Force | Out-Null
Set-ItemProperty -Path $classKey -Name "(Default)" -Value "Lite Anvil"
New-Item -Path "$classKey\DefaultIcon" -Force | Out-Null
Set-ItemProperty -Path "$classKey\DefaultIcon" -Name "(Default)" -Value "$ExePath,0"
New-Item -Path "$classKey\shell\open\command" -Force | Out-Null
Set-ItemProperty -Path "$classKey\shell\open\command" -Name "(Default)" -Value "`"$ExePath`" `"%1`""

# Register in Applications
$appsKey = "HKCU:\Software\Classes\Applications\lite-anvil.exe"
New-Item -Path "$appsKey\shell\open\command" -Force | Out-Null
Set-ItemProperty -Path "$appsKey\shell\open\command" -Name "(Default)" -Value "`"$ExePath`" `"%1`""

# Supported extensions — adds Lite Anvil to "Open With" for each.
$extensions = @(
    # Plain text
    ".txt", ".text", ".log", ".conf", ".cfg", ".ini", ".env",
    # C / C++
    ".c", ".h", ".cpp", ".cxx", ".cc", ".hpp", ".hxx",
    # C# / Java
    ".cs", ".java",
    # Scripting
    ".py", ".pyw", ".rb", ".pl", ".pm", ".sh", ".bash", ".zsh", ".fish",
    ".lua", ".php",
    # Systems
    ".go", ".rs", ".swift", ".kt", ".kts", ".scala", ".zig", ".d", ".dart",
    ".cr", ".gleam", ".jl",
    # Functional
    ".hs", ".lhs", ".ml", ".mli", ".fs", ".fsi", ".fsx",
    ".lisp", ".cl", ".el", ".scm",
    ".clj", ".cljs", ".cljc", ".edn",
    ".ex", ".exs", ".erl",
    ".r", ".R",
    # Web
    ".html", ".htm", ".css", ".scss", ".less",
    ".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx",
    ".vue", ".svelte",
    # Data / Config
    ".xml", ".svg", ".yaml", ".yml", ".toml", ".json", ".jsonc",
    ".csv", ".tsv", ".sql",
    # Documentation
    ".md", ".markdown",
    # Assembly / Low-level
    ".asm", ".s", ".S",
    # PowerShell
    ".ps1", ".psm1", ".psd1",
    # Build
    ".cmake", ".mk", ".meson",
    # Misc
    ".diff", ".patch",
    ".gitignore", ".gitattributes", ".editorconfig",
    ".dockerignore"
)

foreach ($ext in $extensions) {
    $openWithKey = "HKCU:\Software\Classes\$ext\OpenWithProgids"
    New-Item -Path $openWithKey -Force | Out-Null
    Set-ItemProperty -Path $openWithKey -Name $AppId -Value "" -Type String
}

Write-Host "Registered $($extensions.Count) file extensions."
Write-Host "Done. Lite Anvil will appear in 'Open With' for supported files."
