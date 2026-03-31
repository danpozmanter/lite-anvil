use mlua::prelude::*;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Detects the project test framework from file markers in the project root.
fn detect_runner(lua: &Lua) -> LuaResult<Option<LuaTable>> {
    let core = require_table(lua, "core")?;
    let root_project: Option<LuaFunction> = core.get("root_project")?;
    let project_path = match root_project {
        Some(f) => match f.call::<Option<LuaTable>>(())? {
            Some(p) => p.get::<Option<String>>("path")?,
            None => None,
        },
        None => None,
    };
    let project_path = match project_path {
        Some(p) => p,
        None => return Ok(None),
    };
    let system: LuaTable = lua.globals().get("system")?;
    let get_file_info: LuaFunction = system.get("get_file_info")?;
    let project_path_ref = &project_path;
    let exists = |name: &str| -> LuaResult<bool> {
        let path = format!("{}/{}", project_path_ref, name);
        let info: LuaValue = get_file_info.call(path)?;
        Ok(!matches!(info, LuaValue::Nil))
    };
    let runner = lua.create_table()?;
    runner.set("project_path", project_path.as_str())?;
    if exists("Cargo.toml")? {
        runner.set("type", "cargo")?;
        runner.set("run_all", "cargo test")?;
    } else if exists("package.json")? {
        runner.set("type", "node")?;
        if exists("node_modules/.bin/vitest")? {
            runner.set("run_all", "npx vitest run")?;
        } else if exists("node_modules/.bin/jest")? {
            runner.set("run_all", "npx jest")?;
        } else {
            runner.set("run_all", "npm test")?;
        }
    } else if exists("pytest.ini")? || exists("conftest.py")? {
        runner.set("type", "pytest")?;
        runner.set("run_all", "python -m pytest -v")?;
    } else if exists("pyproject.toml")? || exists("setup.py")? || exists("setup.cfg")? {
        // Check if pytest is configured in pyproject.toml, otherwise fall back to unittest
        let has_pytest = if exists("pyproject.toml")? {
            let toml_path = format!("{}/pyproject.toml", project_path_ref);
            let content = std::fs::read_to_string(&toml_path).unwrap_or_default();
            content.contains("[tool.pytest") || content.contains("pytest")
        } else {
            false
        };
        if has_pytest {
            runner.set("type", "pytest")?;
            runner.set("run_all", "python -m pytest -v")?;
        } else {
            runner.set("type", "unittest")?;
            runner.set("run_all", "python -m unittest discover -v")?;
        }
    } else if exists("tests")? || exists("test")? {
        // Bare Python project with a tests/ directory
        if exists("tests/conftest.py")? || exists("test/conftest.py")? {
            runner.set("type", "pytest")?;
            runner.set("run_all", "python -m pytest -v")?;
        } else {
            runner.set("type", "unittest")?;
            runner.set("run_all", "python -m unittest discover -v")?;
        }
    } else if exists("go.mod")? {
        runner.set("type", "go")?;
        runner.set("run_all", "go test ./...")?;
    } else if has_extension(lua, project_path_ref, "sln")?
        || has_extension(lua, project_path_ref, "csproj")?
        || has_extension(lua, project_path_ref, "fsproj")?
    {
        runner.set("type", "dotnet")?;
        runner.set("run_all", "dotnet test")?;
    } else if exists("build.gradle")? || exists("build.gradle.kts")? {
        runner.set("type", "gradle")?;
        if exists("gradlew")? {
            runner.set("run_all", "./gradlew test")?;
        } else {
            runner.set("run_all", "gradle test")?;
        }
    } else if exists("pom.xml")? {
        runner.set("type", "maven")?;
        if exists("mvnw")? {
            runner.set("run_all", "./mvnw test")?;
        } else {
            runner.set("run_all", "mvn test")?;
        }
    } else if exists("Makefile")? || exists("makefile")? {
        runner.set("type", "make")?;
        runner.set("run_all", "make test")?;
    } else {
        return Ok(None);
    }
    Ok(Some(runner))
}

/// Checks if any file with the given extension exists in the project root directory.
fn has_extension(lua: &Lua, project_path: &str, ext: &str) -> LuaResult<bool> {
    let system: LuaTable = lua.globals().get("system")?;
    let list_dir: LuaFunction = system.get("list_dir")?;
    let entries: LuaValue = list_dir.call(project_path.to_owned())?;
    if let LuaValue::Table(entries) = entries {
        let suffix = format!(".{}", ext);
        for entry in entries.sequence_values::<LuaTable>() {
            let entry = entry?;
            let name: String = entry.get("filename").unwrap_or_default();
            if name.ends_with(&suffix) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Derives a Cargo module filter from an absolute file path and the project root.
/// e.g. `/home/user/proj/src/foo/bar.rs` with root `/home/user/proj`
/// becomes `foo::bar` (stripping src/ prefix and .rs suffix).
fn cargo_module_filter(file_path: &str, project_path: &str) -> Option<String> {
    let relative = file_path.strip_prefix(project_path)?.trim_start_matches('/');
    // Handle workspace members: strip `crate_name/src/` or just `src/`
    let after_src = if let Some(pos) = relative.find("/src/") {
        &relative[pos + 5..]
    } else if let Some(rest) = relative.strip_prefix("src/") {
        rest
    } else {
        relative
    };
    let stem = after_src.strip_suffix(".rs")?;
    if stem == "lib" || stem == "main" {
        return None;
    }
    // Strip trailing /mod for mod.rs files (e.g. editor/mod -> editor)
    let stem = stem.strip_suffix("/mod").unwrap_or(stem);
    if stem.is_empty() {
        return None;
    }
    Some(stem.replace('/', "::"))
}

/// Builds the file-scoped test command for a given runner type.
fn file_test_command(runner_type: &str, file_path: &str, project_path: &str) -> Option<String> {
    match runner_type {
        "cargo" => {
            let filter = cargo_module_filter(file_path, project_path)?;
            Some(format!("cargo test {}", filter))
        }
        "pytest" => Some(format!("python -m pytest -v {}", file_path)),
        "unittest" => {
            // Convert file path to dotted module: tests/test_foo.py -> tests.test_foo
            let rel = file_path.strip_prefix(project_path)?.trim_start_matches('/');
            let module = rel.strip_suffix(".py")?.replace('/', ".");
            Some(format!("python -m unittest -v {}", module))
        }
        "go" => {
            let dir = file_path.rsplit_once('/')?.0;
            let rel = dir.strip_prefix(project_path)?.trim_start_matches('/');
            if rel.is_empty() {
                Some("go test -v .".to_owned())
            } else {
                Some(format!("go test -v ./{}", rel))
            }
        }
        "node" => Some(format!("npx vitest run {}", file_path)),
        "dotnet" => Some(format!("dotnet test --filter FullyQualifiedName~{}", {
            let name = file_path.rsplit('/').next().unwrap_or(file_path);
            name.strip_suffix(".cs")
                .or_else(|| name.strip_suffix(".fs"))
                .unwrap_or(name)
        })),
        "gradle" => Some(format!("./gradlew test --tests \"*{}*\"", {
            let name = file_path.rsplit('/').next().unwrap_or(file_path);
            name.strip_suffix(".java")
                .or_else(|| name.strip_suffix(".kt"))
                .unwrap_or(name)
        })),
        "maven" => Some(format!("mvn test -Dtest=\"{}\"", {
            let name = file_path.rsplit('/').next().unwrap_or(file_path);
            name.strip_suffix(".java")
                .or_else(|| name.strip_suffix(".kt"))
                .unwrap_or(name)
        })),
        _ => None,
    }
}

/// Gets the active document's absolute filename.
fn active_file_path(lua: &Lua) -> LuaResult<Option<String>> {
    let core = require_table(lua, "core")?;
    let view: Option<LuaTable> = core.get("active_view")?;
    Ok(view
        .and_then(|v| v.get::<Option<LuaTable>>("doc").ok().flatten())
        .and_then(|d| d.get::<Option<String>>("abs_filename").ok().flatten()))
}

/// Opens a terminal pane running the given test command.
/// The process runs to completion; the pane stays open so the user can read output.
/// Closing the pane does not trigger a "terminate?" warning since the process is dead.
fn run_in_terminal(lua: &Lua, command: &str, cwd: &str, title: &str) -> LuaResult<()> {
    let terminal_view = require_table(lua, "plugins.terminal.view")?;
    let wrapped = format!(
        "{cmd}; __EXIT=$?; echo ''; \
         if [ $__EXIT -eq 0 ]; then \
           echo '-- Tests passed (exit 0) --'; \
         else \
           echo \"-- Tests failed (exit $__EXIT) --\"; \
         fi; \
         exit $__EXIT",
        cmd = command,
    );
    let cmd = lua.create_table()?;
    cmd.raw_set(1, "sh")?;
    cmd.raw_set(2, "-c")?;
    cmd.raw_set(3, wrapped)?;
    let view: LuaTable = terminal_view.call_function(
        "open",
        (
            cwd.to_owned(),
            cmd,
            title.to_owned(),
            "bottom".to_owned(),
        ),
    )?;
    view.set("keep_open", true)?;
    Ok(())
}

/// Registers the `plugins.test_runner` module.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;
    preload.set(
        "plugins.test_runner",
        lua.create_function(|lua, ()| {
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let common = require_table(lua, "core.common")?;
            let defaults = lua.create_table()?;

            let spec = lua.create_table()?;
            spec.set("name", "Test Runner")?;

            let cmd_entry = lua.create_table()?;
            cmd_entry.set("label", "Custom Test Command")?;
            cmd_entry.set(
                "description",
                "Override the auto-detected test command (leave blank for auto-detect).",
            )?;
            cmd_entry.set("path", "custom_command")?;
            cmd_entry.set("type", "string")?;
            cmd_entry.set("default", "")?;
            spec.push(cmd_entry)?;
            defaults.set("config_spec", spec)?;
            defaults.set("custom_command", "")?;

            let merged: LuaTable = common
                .call_function("merge", (defaults, plugins.get::<LuaValue>("test_runner")?))?;
            plugins.set("test_runner", merged)?;

            let command = require_table(lua, "core.command")?;
            let cmds = lua.create_table()?;

            cmds.set(
                "test:run-all",
                lua.create_function(|lua, ()| {
                    let config = require_table(lua, "core.config")?;
                    let plugins: LuaTable = config.get("plugins")?;
                    let tr_cfg: LuaTable = plugins.get("test_runner")?;
                    let custom: String = tr_cfg.get("custom_command").unwrap_or_default();
                    if !custom.is_empty() {
                        let core = require_table(lua, "core")?;
                        let cwd = match core.get::<Option<LuaFunction>>("root_project")? {
                            Some(f) => match f.call::<Option<LuaTable>>(())? {
                                Some(p) => p.get::<String>("path").unwrap_or_else(|_| ".".into()),
                                None => ".".into(),
                            },
                            None => ".".into(),
                        };
                        return run_in_terminal(lua, &custom, &cwd, "Test: All");
                    }
                    let runner = detect_runner(lua)?;
                    match runner {
                        Some(r) => {
                            let cmd: String = r.get("run_all")?;
                            let cwd: String = r.get("project_path")?;
                            run_in_terminal(lua, &cmd, &cwd, "Test: All")
                        }
                        None => {
                            let core = require_table(lua, "core")?;
                            core.get::<LuaFunction>("warn")?
                                .call::<()>("No test runner detected for this project")?;
                            Ok(())
                        }
                    }
                })?,
            )?;

            cmds.set(
                "test:run-file",
                lua.create_function(|lua, ()| {
                    let runner = detect_runner(lua)?;
                    let runner = match runner {
                        Some(r) => r,
                        None => {
                            let core = require_table(lua, "core")?;
                            core.get::<LuaFunction>("warn")?
                                .call::<()>("No test runner detected for this project")?;
                            return Ok(());
                        }
                    };
                    let rtype: String = runner.get("type")?;
                    let cwd: String = runner.get("project_path")?;
                    let file_path = active_file_path(lua)?;
                    let cmd = match file_path {
                        Some(ref fp) => file_test_command(&rtype, fp, &cwd)
                            .unwrap_or_else(|| {
                                runner.get::<String>("run_all").unwrap_or_default()
                            }),
                        None => runner.get("run_all")?,
                    };
                    let title = match file_path {
                        Some(ref fp) => {
                            let name = fp.rsplit('/').next().unwrap_or(fp);
                            format!("Test: {}", name)
                        }
                        None => "Test: All".into(),
                    };
                    run_in_terminal(lua, &cmd, &cwd, &title)
                })?,
            )?;

            command.call_function::<()>("add", (LuaValue::Nil, cmds))?;

            let keymap = require_table(lua, "core.keymap")?;
            let bindings = lua.create_table()?;
            bindings.set("ctrl+shift+r", "test:run-all")?;
            keymap.call_function::<()>("add", bindings)?;

            Ok(LuaValue::Boolean(true))
        })?,
    )
}
