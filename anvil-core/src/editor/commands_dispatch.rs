// Command dispatch body. This file is NOT a module — it is `include!()`d
// verbatim from `main_loop::run()` as the body of the `dispatch_command!`
// macro, so every bare identifier below (docs, nag, cmdview_*, palette_*,
// sidebar_*, lsp_state, terminal, style, config, subsystems, userdir_path,
// project_root, ...) resolves against run()'s local scope. Wrapping
// those ~60 state variables in a struct so this could become a plain
// function would be a much larger refactor; the include!() split is the
// minimum change that gets the dispatch logic out of main_loop.rs while
// keeping all semantics identical.
//
// Add a new command by editing exactly one match arm here.

{
let block_large_file_edit = is_document_mutation_command(&cmd)
    && config.large_file.read_only
    && docs
        .get(active_tab)
        .map(|doc| {
            crate::editor::open_doc::exceeds_soft_limit(doc, config.large_file.soft_limit_mb)
        })
        .unwrap_or(false);
if block_large_file_edit {
    info_message = Some((
        "Large file is read-only by configuration".to_string(),
        Instant::now(),
    ));
} else {
match cmd.as_str() {
"core:quit" => {
    if single_file_mode && docs.iter().any(doc_is_modified) {
        nag = Nag::UnsavedChanges { message: nag_msg_quit(&docs), tab_to_close: None };
    } else {
        quit = true;
    }
}
"core:force-quit" => {
    quit = true;
}
"core:new-window" => {
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(exe);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Detach from the parent so the new window survives this process
        // and doesn't inherit the controlling terminal / session.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Double-fork: the child we spawn forks the real new window as a
            // grandchild and exits at once. The grandchild reparents to
            // init/launchd and is reaped there, so it never lingers as a zombie
            // of this editor even if the new window outlives or predeceases us.
            // setsid on the grandchild detaches it from the controlling terminal.
            // SAFETY: only fork/setsid/_exit run post-fork, all async-signal-safe.
            unsafe {
                cmd.pre_exec(|| match libc::fork() {
                    -1 => Err(std::io::Error::last_os_error()),
                    0 => {
                        libc::setsid();
                        Ok(())
                    }
                    _ => libc::_exit(0),
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        match cmd.spawn() {
            Ok(child) => {
                // On unix the spawned child is the intermediate fork, which exits
                // immediately; waiting on it reaps it at once. On other platforms
                // the detached child manages its own lifetime, so just release it.
                #[cfg(unix)]
                {
                    let mut child = child;
                    let _ = child.wait();
                }
                #[cfg(not(unix))]
                drop(child);
            }
            Err(e) => log_to_file(userdir, &format!("core:new-window spawn failed: {e}")),
        }
    }
}
"core:find-command" => {
    palette_active = true;
    palette_query.clear();
    palette_results = all_commands.clone();
    palette_selected = 0;
}
"notes:delete-current" => {
    if subsystems.has_notes_mode()
        && let Some(doc) = docs.get(active_tab)
        && !doc.path.is_empty()
    {
        let path = doc.path.clone();
        if std::fs::remove_file(&path).is_ok() {
            autoreload.unwatch(&path);
            docs.remove(active_tab);
            if docs.is_empty() {
                active_tab = 0;
            } else if active_tab >= docs.len() {
                active_tab = docs.len() - 1;
            }
            if subsystems.has_sidebar() && !project_root.is_empty() {
                sidebar_entries =
                    scan_for_sidebar(true, &project_root, sidebar_show_hidden);
            }
        }
    }
}
"doc:save-all" => {
    let atomic = config.files.atomic_save;
    let mut saved = 0usize;
    let mut failed = 0usize;
    for doc in docs.iter_mut() {
        if doc.path.is_empty() || !doc_is_modified(doc) {
            continue;
        }
        let Some(buf_id) = doc.view.buffer_id else {
            continue;
        };
        let path = doc.path.clone();
        let outcome = buffer::with_buffer(buf_id, |b| {
            Ok(buffer::save_file(b, &path, b.crlf, atomic).map(|()| b.change_id))
        });
        match outcome {
            Ok(Ok(id)) => {
                doc.saved_change_id = id;
                doc.saved_signature =
                    buffer::with_buffer(buf_id, |b| Ok(buffer::content_signature(&b.lines)))
                        .unwrap_or(0);
                if subsystems.has_git() {
                    crate::editor::git::start_diff(&path);
                }
                saved += 1;
            }
            _ => failed += 1,
        }
    }
    info_message = Some((
        if failed > 0 {
            format!("Saved {saved} file(s), {failed} failed")
        } else {
            format!("Saved {saved} file(s)")
        },
        Instant::now(),
    ));
}
"doc:copy-path" => {
    if let Some(doc) = docs.get(active_tab)
        && !doc.path.is_empty()
    {
        crate::window::set_clipboard_text(&doc.path);
    }
}
"doc:copy-relative-path" => {
    if let Some(doc) = docs.get(active_tab)
        && !doc.path.is_empty()
    {
        let rel = std::path::Path::new(&doc.path)
            .strip_prefix(&project_root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| doc.path.clone());
        crate::window::set_clipboard_text(&rel);
    }
}
"doc:reopen-closed-tab" => {
    // Pop toward the most recently closed file that is not already open and
    // still exists on disk.
    while let Some(path) = closed_tabs.pop() {
        if docs.iter().any(|d| d.path == path) || !std::path::Path::new(&path).exists() {
            continue;
        }
        if open_file_into(&path, &mut docs, use_git()) {
            active_tab = docs.len() - 1;
            autoreload.watch(&path);
            remember_recent_file(&mut recent_files, &path, userdir_path);
        }
        break;
    }
}
"doc:match-bracket" => {
    if let Some(doc) = docs.get(active_tab)
        && let Some(buf_id) = doc.view.buffer_id
    {
        let _ = buffer::with_buffer_mut(buf_id, |b| {
            let line = *b.selections.get(2).unwrap_or(&1);
            let col = *b.selections.get(3).unwrap_or(&1);
            if let Some((l1, c1, l2, c2)) =
                crate::editor::picker::bracket_pair(&b.lines, line, col)
            {
                let (tl, tc) = if (line, col) == (l1, c1) {
                    (l2, c2)
                } else {
                    (l1, c1)
                };
                b.selections = vec![tl, tc, tl, tc];
            }
            Ok(())
        });
    }
}
"doc:convert-indentation" => {
    if let Some(doc) = docs.get_mut(active_tab) {
        // Flip the document between tabs and spaces, rewriting existing
        // leading indentation to match the new style.
        let to_tabs = doc.indent_type != "hard";
        doc.indent_type = if to_tabs { "hard" } else { "soft" }.to_string();
        let size = doc.indent_size.max(1);
        let buf_id = doc.view.buffer_id;
        if let Some(buf_id) = buf_id {
            let _ = buffer::with_buffer_mut(buf_id, |b| {
                buffer::push_undo(b);
                for line in b.lines.iter_mut() {
                    let trimmed = line.trim_start_matches([' ', '\t']);
                    let indent_len = line.len() - trimmed.len();
                    let mut cols = 0usize;
                    for ch in line[..indent_len].chars() {
                        cols += if ch == '\t' { size } else { 1 };
                    }
                    let new_indent = if to_tabs {
                        format!("{}{}", "\t".repeat(cols / size), " ".repeat(cols % size))
                    } else {
                        " ".repeat(cols)
                    };
                    *line = format!("{new_indent}{trimmed}");
                }
                b.change_id += 1;
                Ok(())
            });
        }
        info_message = Some((
            if to_tabs {
                "Indentation: tabs".to_string()
            } else {
                "Indentation: spaces".to_string()
            },
            Instant::now(),
        ));
    }
}
"doc:toggle-line-endings" => {
    if let Some(doc) = docs.get(active_tab)
        && let Some(buf_id) = doc.view.buffer_id
    {
        let crlf = buffer::with_buffer_mut(buf_id, |b| {
            b.crlf = !b.crlf;
            Ok(b.crlf)
        })
        .unwrap_or(false);
        info_message = Some((
            if crlf {
                "Line endings: CRLF (saved on next write)".to_string()
            } else {
                "Line endings: LF (saved on next write)".to_string()
            },
            Instant::now(),
        ));
    }
}
"core:new-doc" => {
    if !crate::editor::main_loop::can_open_another_tab(docs.len()) {
        info_message = Some(("Open tab limit reached".to_string(), Instant::now()));
    } else if subsystems.has_notes_mode() && !project_root.is_empty() {
        // Notes mode: create a new "Note N.md" on disk and open it,
        // matching NoteSquirrel's lifecycle. The number is the smallest
        // integer >= existing-note-count + 1 that doesn't collide.
        let dir = std::path::PathBuf::from(&project_root);
        let mut count = std::fs::read_dir(&dir)
            .map(|it| {
                it.flatten()
                    .filter(|e| {
                        e.path()
                            .extension()
                            .and_then(|x| x.to_str())
                            .map(|x| x.eq_ignore_ascii_case("md"))
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
            + 1;
        let new_path = loop {
            let candidate = dir.join(format!("Note {count}.md"));
            if !candidate.exists() {
                break candidate;
            }
            count += 1;
        };
        if std::fs::write(&new_path, "").is_ok() {
            let path_str = new_path.to_string_lossy().to_string();
            if open_file_into(&path_str, &mut docs, use_git()) {
                active_tab = docs.len() - 1;
                autoreload.watch(&path_str);
                if subsystems.has_sidebar() {
                    sidebar_entries = scan_for_sidebar(true, &project_root, sidebar_show_hidden);
                }
            }
        }
    } else {
        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
        let mut dv = DocView::new();
        dv.buffer_id = Some(buf_id);
        docs.push(OpenDoc {
            view: dv,
            path: String::new(),
            name: "untitled".to_string(),
            saved_change_id: 1,
            saved_signature: buffer::content_signature(&["\n".to_string()]),
            indent_type: "soft".to_string(),
            indent_size: 2,
            git_changes: std::collections::HashMap::new(),
            cached_render: std::sync::Arc::new(Vec::new()),
            cached_change_id: -1,
            cached_scroll_y: -1.0,
            cached_hint_count: 0,
            cached_rect_w: -1.0,
            cached_rect_h: -1.0,
            dirty_cache: std::cell::Cell::new(None),
            token_cache: std::cell::RefCell::new(
                crate::editor::open_doc::TokenCache::default(),
            ),
            preview: crate::editor::markdown_preview::MarkdownPreviewState::default(),
        });
        active_tab = docs.len() - 1;
    }
}
"root:close" => {
    if !docs.is_empty() {
        if doc_is_modified(&docs[active_tab]) {
            nag = Nag::UnsavedChanges { message: nag_msg_close(&docs[active_tab].name), tab_to_close: Some(active_tab) };
        } else {
            if let Some(d) = docs.get(active_tab) {
                autoreload.unwatch(&d.path);
            }
            docs.remove(active_tab);
            if docs.is_empty() {
                active_tab = 0;
            } else if active_tab >= docs.len() {
                active_tab = docs.len() - 1;
            }
        }
    }
}
"core:close-project-folder" => {
    if subsystems.has_sidebar() {
        if docs.iter().any(doc_is_modified) {
            nag = Nag::UnsavedChanges { message: nag_msg_quit(&docs), tab_to_close: None };
        } else {
            save_project_session(userdir_path, &project_root, &docs, active_tab);
            save_expanded_folders(
                &sidebar_entries,
                userdir_path,
                &project_session_key(&project_root),
            );
            for d in &docs { autoreload.unwatch(&d.path); }
            docs.clear();
            pending_render_cache = None;
            active_tab = 0;
            project_root = String::new();
            sidebar_entries = Vec::new();
            sidebar_visible = false;
        }
    }
}
"root:close-all" => {
    if !single_file_mode {
        if docs.iter().any(doc_is_modified) {
            nag = Nag::UnsavedChanges { message: nag_msg_quit(&docs), tab_to_close: None };
        } else {
            for d in &docs { autoreload.unwatch(&d.path); }
            docs.clear();
            active_tab = 0;
        }
    }
}
"root:close-all-others" => {
    if !single_file_mode {
        let keep = active_tab;
        for i in (0..docs.len()).rev() {
            if i != keep {
                autoreload.unwatch(&docs[i].path);
                docs.remove(i);
            }
        }
        active_tab = 0;
    }
}
"root:close-or-quit" => {
    if docs.is_empty() {
        quit = true;
    } else if doc_is_modified(&docs[active_tab]) {
        nag = Nag::UnsavedChanges { message: nag_msg_close(&docs[active_tab].name), tab_to_close: Some(active_tab) };
    } else {
        autoreload.unwatch(&docs[active_tab].path);
        docs.remove(active_tab);
        if docs.is_empty() {
            quit = true;
        } else if active_tab >= docs.len() {
            active_tab = docs.len() - 1;
        }
    }
}
"root:switch-to-next-tab" => {
    if !single_file_mode && !docs.is_empty() {
        active_tab = (active_tab + 1) % docs.len();
    }
}
"root:switch-to-previous-tab" => {
    if !single_file_mode && !docs.is_empty() {
        active_tab = if active_tab == 0 { docs.len() - 1 } else { active_tab - 1 };
    }
}
"root:toggle-sidebar" | "core:toggle-sidebar" => {
    if subsystems.has_sidebar() {
        sidebar_visible = !sidebar_visible;
    }
}
"core:toggle-terminal" => {
    if subsystems.has_terminal() {
        terminal.visible = !terminal.visible;
        if terminal.visible && terminal.terminals.is_empty() {
            let active_doc_path =
                docs.get(active_tab).map(|d| d.path.as_str()).unwrap_or("");
            let cwd = crate::editor::terminal_panel::resolve_terminal_cwd(
                active_doc_path,
                &project_root,
            );
            if terminal.spawn(&cwd) {
                let n = terminal.terminals.len();
                let cd_payload =
                    crate::editor::terminal_panel::terminal_cd_payload(&cwd);
                if let Some(t) = terminal.active_terminal() {
                    t.title = crate::editor::terminal_panel::terminal_title(n, &cwd);
                    let _ = t.inner.write(cd_payload.as_bytes());
                }
                log_to_file(userdir, "Terminal spawned via toggle");
            }
        }
        terminal.focused = terminal.visible;
    }
}
"core:new-terminal" => {
    if subsystems.has_terminal() {
        let active_doc_path =
            docs.get(active_tab).map(|d| d.path.as_str()).unwrap_or("");
        let cwd = crate::editor::terminal_panel::resolve_terminal_cwd(
            active_doc_path,
            &project_root,
        );
        if terminal.spawn(&cwd) {
            let n = terminal.terminals.len();
            let cd_payload =
                crate::editor::terminal_panel::terminal_cd_payload(&cwd);
            if let Some(t) = terminal.active_terminal() {
                t.title = crate::editor::terminal_panel::terminal_title(n, &cwd);
                let _ = t.inner.write(cd_payload.as_bytes());
            }
            log_to_file(userdir, &format!("New terminal {n} spawned"));
        }
    }
}
"test:run-all" => {
    if subsystems.has_terminal() {
        let active_path =
            docs.get(active_tab).map(|d| d.path.as_str()).unwrap_or("");
        if let Some(runner) = crate::editor::test_runner::detect_runner_with_fallback(
            &project_root,
            active_path,
        ) {
            crate::editor::test_runner::launch_in_terminal(
                &mut terminal,
                &runner.project_path,
                &runner.run_all,
                "Test: All",
            );
            terminal.visible = true;
            terminal.focused = true;
        } else {
            info_message = Some((
                "No test runner detected for this project.".to_string(),
                Instant::now(),
            ));
        }
    }
}
"test:run-in-current-file" => {
    if subsystems.has_terminal() {
        let doc_path = docs
            .get(active_tab)
            .map(|d| d.path.clone())
            .unwrap_or_default();
        if doc_path.is_empty() {
            info_message = Some((
                "Save the file first to scope tests to it.".to_string(),
                Instant::now(),
            ));
        } else if let Some(runner) =
            crate::editor::test_runner::detect_runner_with_fallback(
                &project_root,
                &doc_path,
            )
        {
            let cmd = crate::editor::test_runner::file_test_command(
                &runner, &doc_path,
            )
            .unwrap_or_else(|| runner.run_all.clone());
            let title = doc_path
                .rsplit('/')
                .next()
                .map(|n| format!("Test: {n}"))
                .unwrap_or_else(|| "Test: file".to_string());
            crate::editor::test_runner::launch_in_terminal(
                &mut terminal,
                &runner.project_path,
                &cmd,
                &title,
            );
            terminal.visible = true;
            terminal.focused = true;
        } else {
            info_message = Some((
                "No test runner detected for this project.".to_string(),
                Instant::now(),
            ));
        }
    }
}
"test:run-single" => {
    if subsystems.has_terminal() {
        if let Some((ref doc_path, ref test_name)) = pending_single_test {
            if let Some(runner) =
                crate::editor::test_runner::detect_runner_with_fallback(
                    &project_root,
                    doc_path,
                )
            {
                let cmd = crate::editor::test_runner::single_test_command(
                    &runner, doc_path, test_name,
                )
                .unwrap_or_else(|| runner.run_all.clone());
                let title = format!("Test: {test_name}");
                crate::editor::test_runner::launch_in_terminal(
                    &mut terminal,
                    &runner.project_path,
                    &cmd,
                    &title,
                );
                terminal.visible = true;
                terminal.focused = true;
            }
        }
        pending_single_test = None;
    }
}
"core:close-terminal" => {
    if subsystems.has_terminal() {
        terminal.close_active();
        crate::window::force_invalidate();
    }
}
"core:toggle-minimap" => {
    minimap_visible = !minimap_visible;
}
"core:toggle-markdown-preview" => {
    if let Some(doc) = docs.get_mut(active_tab) {
        let is_md = doc.path.ends_with(".md")
            || doc.path.ends_with(".markdown")
            || doc.name.ends_with(".md")
            || doc.name.ends_with(".markdown");
        if is_md {
            doc.preview.enabled = !doc.preview.enabled;
            if doc.preview.enabled {
                // Force a reparse + relayout on first draw.
                doc.preview.cached_change_id = -1;
                doc.preview.cached_width = 0.0;
                doc.preview.layout.clear();
                doc.preview.scroll_y = 0.0;
                doc.preview.target_scroll_y = 0.0;
            }
        } else {
            info_message = Some((
                "Markdown preview: active file is not a markdown document"
                    .to_string(),
                Instant::now(),
            ));
        }
    }
}
"core:toggle-line-wrapping" => {
    line_wrapping = !line_wrapping;
    let _ = crate::editor::storage::save_text(
        userdir_path,
        "session",
        "line_wrapping",
        if line_wrapping { "true" } else { "false" },
    );
    // Invalidate the per-tab render cache so wrapped and
    // un-wrapped layouts don't get re-used across toggles.
    for d in docs.iter_mut() {
        d.cached_render = std::sync::Arc::new(Vec::new());
        d.cached_change_id = -1;
    }
    // Reset horizontal scroll when turning wrap on so the
    // right edge of wrapped lines is always visible.
    if line_wrapping {
        if let Some(doc) = docs.get_mut(active_tab) {
            doc.view.scroll_x = 0.0;
            doc.view.target_scroll_x = 0.0;
        }
    }
}
"core:toggle-whitespace" => {
    if let Some(doc) = docs.get_mut(active_tab) {
        doc.view.show_whitespace = !doc.view.show_whitespace;
    }
}
"core:toggle-hidden-files" => {
    if subsystems.has_sidebar() {
        sidebar_show_hidden = !sidebar_show_hidden;
        sidebar_entries = scan_for_sidebar(subsystems.has_notes_mode(), &project_root, sidebar_show_hidden);
        restore_expanded_folders(
            &mut sidebar_entries,
            userdir_path,
            sidebar_show_hidden,
            &project_session_key(&project_root),
        );
        let label = if sidebar_show_hidden { "Showing hidden files" } else { "Hiding hidden files" };
        info_message = Some((label.to_string(), Instant::now()));
    }
}
"core:check-for-updates" => {
    if subsystems.has_update_check() && update_check_job.is_none() {
        info_message = Some(("Checking for updates...".to_string(), Instant::now()));
        // curl blocks until its --max-time; run it off the UI thread and
        // surface the outcome from the per-frame poll.
        update_check_job = Some(std::thread::spawn(|| {
            let current = env!("CARGO_PKG_VERSION");
            match std::process::Command::new("curl")
                .args([
                    "-sL",
                    "--max-time",
                    "5",
                    "https://api.github.com/repos/danpozmanter/lite-anvil/releases/latest",
                ])
                .output()
            {
                Ok(output) if output.status.success() => {
                    let body = String::from_utf8_lossy(&output.stdout);
                    // Parse the tag_name from the JSON response.
                    let latest = body
                        .split("\"tag_name\"")
                        .nth(1)
                        .and_then(|s| s.split('"').nth(1))
                        .map(|s| s.trim_start_matches('v'))
                        .unwrap_or("");
                    if latest.is_empty() {
                        "Could not determine latest version".to_string()
                    } else if latest == current {
                        format!("Up to date (v{current})")
                    } else if semver_gt(latest, current) {
                        format!("New version available: v{latest} (current: v{current})")
                    } else {
                        format!("Up to date (v{current})")
                    }
                }
                _ => "Update check failed (no network or curl not found)".to_string(),
            }
        }));
    }
}
"core:cycle-theme" => {
    if !available_themes.is_empty() {
        current_theme_idx = (current_theme_idx + 1) % available_themes.len();
        let new_theme = &available_themes[current_theme_idx];
        let tp = Path::new(datadir)
            .join("assets")
            .join("themes")
            .join(format!("{new_theme}.json"))
            .to_string_lossy()
            .into_owned();
        if let Ok(palette) = crate::editor::style::load_theme_palette(&tp) {
            apply_theme_to_style(&mut style, &palette);
        }
    }
}
"core:open-user-settings" => {
    let settings_path = Path::new(userdir)
        .join("config.toml")
        .to_string_lossy()
        .into_owned();
    if !std::path::Path::new(&settings_path).exists() {
        let _ = std::fs::write(&settings_path, NativeConfig::default_toml_template());
    }
    if open_file_into(&settings_path, &mut docs, use_git()) {
        active_tab = docs.len() - 1;
    }
}
"about:version" => {
    info_message = Some((env!("CARGO_PKG_VERSION").to_string(), Instant::now()));
}
"core:project-replace" => {
    if subsystems.has_find_in_files() {
        project_replace_active = true;
        project_replace_search.clear();
        project_replace_with.clear();
        project_replace_focus_on_replace = false;
        project_replace_results.clear();
        project_replace_selected = 0;
    }
}
"core:project-search" => {
    if subsystems.has_find_in_files() {
        project_search_active = true;
        project_search_query.clear();
        project_search_results.clear();
        project_search_selected = 0;
    }
}
"core:git-status" => {
    if subsystems.has_git() {
        git_status_active = true;
        git_status_selected = 0;
        // Run git off the UI thread; the result is applied from the per-frame
        // poll so a large repo never freezes the editor.
        if git_status_job.is_none() {
            let root = project_root.clone();
            git_status_job = Some(std::thread::spawn(move || run_git_status(&root)));
        }
    }
}
"git:blame" => {
    if subsystems.has_git() {
        if let Some(doc) = docs.get(active_tab) {
            if !doc.path.is_empty() {
                git_blame_active = !git_blame_active;
                // Blame off the UI thread; the poll fills in the lines.
                if git_blame_active && git_blame_job.is_none() {
                    git_blame_lines.clear();
                    let path = doc.path.clone();
                    git_blame_job = Some(std::thread::spawn(move || run_git_blame(&path)));
                }
            }
        }
    }
}
"git:log" => {
    if subsystems.has_git() {
        if let Some(doc) = docs.get(active_tab) {
            if !doc.path.is_empty() {
                git_log_active = true;
                git_log_selected = 0;
                // Log off the UI thread; the poll fills in the entries.
                if git_log_job.is_none() {
                    let path = doc.path.clone();
                    git_log_job = Some(std::thread::spawn(move || run_git_log(&path)));
                }
            }
        }
    }
}
"core:open-recent" => {
    if subsystems.has_picker() || single_file_mode {
        cmdview_active = true;
        cmdview_mode = CmdViewMode::OpenRecent;
        cmdview_text.clear();
        cmdview_cursor = 0;
        cmdview_label = "Open Recent:".to_string();
        let mut combined: Vec<String> = Vec::new();
        // Folders first so they're visible at the top of the list on
        // projects with many recent files. Nano-Anvil skips the folder
        // list entirely -- it has no project concept.
        if !single_file_mode {
            for p in &recent_projects {
                if !combined.contains(p) {
                    combined.push(p.clone());
                }
            }
        }
        for p in &recent_files {
            if !combined.contains(p) {
                combined.push(p.clone());
            }
        }
        cmdview_suggestions = combined;
        cmdview_selected = 0;
    }
}
"core:open-project-folder" => {
    if subsystems.has_picker() {
        cmdview_active = true;
        cmdview_mode = CmdViewMode::OpenFolder;
        // Always start from the absolute project root so backspace
        // navigation can walk up directories cleanly.
        let abs_root = effective_root(&project_root);
        cmdview_text = dir_with_trailing_sep(&abs_root);
        cmdview_cursor = cmdview_text.len();
        cmdview_label = "Open Folder:".to_string();
        cmdview_suggestions = path_suggest(&cmdview_text, &project_root, true);
        cmdview_selected = 0;
    }
}
"core:open-file" | "core:open-file-from-project" => {
    if subsystems.has_picker() || single_file_mode {
        cmdview_active = true;
        cmdview_mode = CmdViewMode::OpenFile;
        let abs_root = effective_root(&project_root);
        if let Some(doc) = docs.get(active_tab) {
            if let Some(pos) = doc.path.rfind(['/', '\\']) {
                cmdview_text = dir_with_trailing_sep(&doc.path[..pos]);
            } else {
                cmdview_text = dir_with_trailing_sep(&abs_root);
            }
        } else {
            cmdview_text = dir_with_trailing_sep(&abs_root);
        }
        cmdview_cursor = cmdview_text.len();
        cmdview_label = "Open File:".to_string();
        cmdview_suggestions = path_suggest(&cmdview_text, &project_root, false);
        cmdview_selected = 0;
    }
}
"core:find" | "find-replace:find" => {
    find_active = true;
    replace_active = false;
    find_focus_on_replace = false;
    find_query.clear();
    find_matches.clear();
    find_current = None;
    find_in_selection = false;
    find_selection_range = None;
    if let Some(doc) = docs.get(active_tab) {
        find_anchor = doc_cursor(&doc.view);
        // If there's a multi-line selection, auto-enable
        // find-in-selection mode.
        let anchor = doc_anchor(&doc.view);
        let cursor = doc_cursor(&doc.view);
        if anchor.0 != cursor.0 {
            find_in_selection = true;
            let (sl, sc) = if anchor < cursor { anchor } else { cursor };
            let (el, ec) = if anchor < cursor { cursor } else { anchor };
            find_selection_range = Some((sl, sc, el, ec));
        }
    }
}
"core:find-replace" | "find-replace:replace" => {
    find_active = true;
    replace_active = true;
    find_focus_on_replace = false;
    find_query.clear();
    replace_query.clear();
    find_matches.clear();
    find_current = None;
    if let Some(doc) = docs.get(active_tab) {
        find_anchor = doc_cursor(&doc.view);
    }
}
"find-replace:repeat-find" => {
    if let Some(doc) = docs.get_mut(active_tab) {
        let dv = &mut doc.view;
        if find_matches.is_empty() && !find_query.is_empty() {
            let sel = if find_in_selection { find_selection_range } else { None };
            find_matches = compute_find_matches_filtered(
                dv, &find_query, find_use_regex, find_whole_word, find_case_insensitive, sel,
            );
        }
        if !find_matches.is_empty() {
            let (cl, cc) = doc_cursor(dv);
            let idx = find_match_at_or_after(&find_matches, cl, cc)
                .unwrap_or(0);
            find_current = Some(idx);
            select_find_match(dv, find_matches[idx], replace_active);
        }
    }
}
"find-replace:previous-find" => {
    if let Some(doc) = docs.get_mut(active_tab) {
        let dv = &mut doc.view;
        if find_matches.is_empty() && !find_query.is_empty() {
            let sel = if find_in_selection { find_selection_range } else { None };
            find_matches = compute_find_matches_filtered(
                dv, &find_query, find_use_regex, find_whole_word, find_case_insensitive, sel,
            );
        }
        if !find_matches.is_empty() {
            let (al, ac) = doc_anchor(dv);
            let idx = find_match_before(&find_matches, al, ac)
                .unwrap_or(find_matches.len() - 1);
            find_current = Some(idx);
            select_find_match(dv, find_matches[idx], replace_active);
        }
    }
}
"doc:go-to-line" => {
    cmdview_active = true;
    cmdview_mode = CmdViewMode::OpenFile; // reuse mode, Enter parses as line number
    cmdview_text.clear();
    cmdview_cursor = 0;
    cmdview_label = "Go To Line:".to_string();
    cmdview_suggestions.clear();
    cmdview_selected = 0;
}
"doc:save-as" => {
    completion.hide();
    cmdview_active = true;
    cmdview_mode = CmdViewMode::SaveAs;
    if let Some(doc) = docs.get(active_tab) {
        if !doc.path.is_empty() {
            cmdview_text = doc.path.clone();
        } else {
            // Fall back to the user's home directory (via
            // `effective_root`) rather than the cwd when
            // there is no project folder, so Nano-Anvil
            // doesn't default Save As to `/` when launched
            // from a desktop entry without a working dir.
            let abs_root = effective_root(&project_root);
            cmdview_text = dir_with_trailing_sep(&abs_root);
        }
    } else {
        cmdview_text = String::new();
    }
    cmdview_cursor = cmdview_text.len();
    cmdview_label = "Save As:".to_string();
    cmdview_suggestions = path_suggest(&cmdview_text, &project_root, false);
    cmdview_selected = 0;
}
"doc:save" => {
    if let Some(doc) = docs.get_mut(active_tab) {
        if let Some(buf_id) = doc.view.buffer_id {
            let path = doc.path.clone();
            if path.is_empty() {
                // No path yet -- open the Save As text input.
                completion.hide();
                cmdview_active = true;
                cmdview_mode = CmdViewMode::SaveAs;
                let abs_root = effective_root(&project_root);
                cmdview_text = dir_with_trailing_sep(&abs_root);
                cmdview_cursor = cmdview_text.len();
                cmdview_label = "Save As:".to_string();
                cmdview_suggestions =
                    path_suggest(&cmdview_text, &project_root, false);
                cmdview_selected = 0;
            } else {
                // If the parent directory vanished since the
                // file was opened, ask before recreating it.
                let parent_missing = std::path::Path::new(&path)
                    .parent()
                    .map(|p| !p.as_os_str().is_empty() && !p.exists())
                    .unwrap_or(false);
                if parent_missing {
                    let parent_str = std::path::Path::new(&path)
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    nag = Nag::CreateDir { parent: parent_str, save_path: path.clone(), doc_tab: active_tab, from_save_as: false };
                    continue;
                }
                let atomic = config.files.atomic_save;
                // Carry the write result out through the inner `Result` so a
                // failed save reports the real error instead of logging "Saved".
                let saved = buffer::with_buffer(buf_id, |b| {
                    Ok(buffer::save_file(b, &path, b.crlf, atomic).map(|()| b.change_id))
                });
                let save_ok = matches!(saved, Ok(Ok(_)));
                match saved {
                    Ok(Ok(id)) => {
                        doc.saved_change_id = id;
                        doc.saved_signature = buffer::with_buffer(buf_id, |b| Ok(buffer::content_signature(&b.lines))).unwrap_or(0);
                        log_to_file(userdir, &format!("Saved {path}"));
                        // Format on save: the "@save" response re-applies edits
                        // and re-saves the formatted result.
                        if config.lsp.format_on_save
                            && subsystems.has_lsp()
                            && lsp_state.initialized
                            && let Some(tid) = lsp_state.transport_id
                        {
                            let ext = path.rsplit('.').next().unwrap_or("");
                            if ext_to_lsp_filetype(ext)
                                .map(|ft| ft == lsp_state.filetype)
                                .unwrap_or(false)
                            {
                                let uri = path_to_uri(&path);
                                let tab_size = doc.indent_size.max(1);
                                let insert_spaces = doc.indent_type != "hard";
                                let req_id = lsp_state.next_id();
                                lsp_state.pending_requests.insert(
                                    req_id,
                                    "textDocument/formatting@save".to_string(),
                                );
                                lsp_state.pending_request_uris.insert(req_id, uri.clone());
                                let _ = lsp::send_message(
                                    tid,
                                    &lsp_formatting_request(req_id, &uri, tab_size, insert_spaces),
                                );
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        info_message = Some((format!("Save failed: {e}"), Instant::now()));
                        log_to_file(userdir, &format!("Save failed {path}: {e}"));
                    }
                    Err(_) => {
                        info_message =
                            Some(("Save failed: buffer unavailable".to_string(), Instant::now()));
                    }
                }
                if save_ok && subsystems.has_git() {
                    // Off the UI thread: the gutter is filled in when main_loop
                    // applies the result from git::drain_diffs by matching path.
                    crate::editor::git::start_diff(&path);
                }
                if save_ok && subsystems.has_lsp() {
                    let save_ext = path.rsplit('.').next().unwrap_or("");
                    if ext_to_lsp_filetype(save_ext).is_some() {
                        if let Some(tid) = lsp_state.transport_id {
                            if lsp_state.initialized {
                                let uri = path_to_uri(&path);
                                let _ = lsp::send_message(tid, &lsp_did_save(&uri));
                                let line_count = buffer::with_buffer(buf_id, |b| Ok(b.lines.len())).unwrap_or(100);
                                let req_id = lsp_state.next_id();
                                lsp_state.pending_requests.insert(req_id, "textDocument/inlayHint".to_string());
                                lsp_state.inlay_hints.clear();
                                let _ = lsp::send_message(tid, &lsp_inlay_hint_request(req_id, &uri, 0, line_count));
                            }
                        }
                    }
                }
            }
        }
    }
}
"doc:undo" | "doc:redo" => {
    if let Some(doc) = docs.get(active_tab) {
        if let Some(buf_id) = doc.view.buffer_id {
            let _ = buffer::with_buffer_mut(buf_id, |b| {
                if cmd == "doc:undo" { buffer::undo(b); } else { buffer::redo(b); }
                Ok(())
            });
        }
        if subsystems.has_lsp()
            && lsp_state.transport_id.is_some()
            && lsp_state.initialized
        {
            lsp_state.inlay_hints.clear();
            let ext = doc.path.rsplit('.').next().unwrap_or("");
            if !doc.path.is_empty() && ext_to_lsp_filetype(ext).is_some() {
                lsp_state.last_change = Some(Instant::now());
                lsp_state.pending_change_uri = Some(path_to_uri(&doc.path));
                lsp_state.pending_change_version += 1;
            }
        }
    }
}
"doc:cut" => {
    if let Some(doc) = docs.get(active_tab) {
        if let Some(buf_id) = doc.view.buffer_id {
            let _ = buffer::with_buffer_mut(buf_id, |b| {
                let text = buffer::get_selected_text(b);
                if !text.is_empty() {
                    crate::window::set_clipboard_text(&text);
                    buffer::push_undo(b);
                    buffer::delete_selection(b);
                }
                Ok(())
            });
        }
    }
}
"doc:copy" => {
    if let Some(doc) = docs.get(active_tab) {
        if let Some(buf_id) = doc.view.buffer_id {
            let _ = buffer::with_buffer(buf_id, |b| {
                let text = buffer::get_selected_text(b);
                if !text.is_empty() {
                    crate::window::set_clipboard_text(&text);
                }
                Ok(())
            });
        }
    }
}
"doc:paste" => {
    if let Some(doc) = docs.get_mut(active_tab) {
        if let Some(buf_id) = doc.view.buffer_id {
            if let Some(text) = crate::window::get_clipboard_text() {
                let text = if config.format_on_paste {
                    convert_paste_indent(
                        &text,
                        &doc.indent_type,
                        doc.indent_size,
                    )
                } else {
                    text
                };
                let _ = buffer::with_buffer_mut(buf_id, |b| {
                    insert_text_at_caret(b, &text);
                    Ok(())
                });
            }
        }
    }
}
"doc:insert-list-item" | "doc:insert-checkbox-item" => {
    // NoteSquirrel-style markdown helpers: insert a "- " or "- [ ] "
    // line, inheriting the indent of the previous line if it was
    // already a bulleted/checkbox item. If the cursor's line is blank,
    // the marker is inserted on that line; otherwise a newline is
    // pushed first.
    let marker = if cmd == "doc:insert-checkbox-item" {
        "- [ ] "
    } else {
        "- "
    };
    if let Some(doc) = docs.get_mut(active_tab)
        && let Some(buf_id) = doc.view.buffer_id
    {
        let _ = buffer::with_buffer_mut(buf_id, |b| {
            let line_idx = b.selections.get(2).copied().unwrap_or(1).saturating_sub(1);
            let col = b.selections.get(3).copied().unwrap_or(1).saturating_sub(1);
            if line_idx >= b.lines.len() {
                return Ok(());
            }
            let prev_indent: String = if line_idx > 0 {
                let prev = &b.lines[line_idx - 1];
                let trimmed = prev.trim_start();
                if trimmed.starts_with("- ")
                    || trimmed.starts_with("* ")
                    || trimmed.starts_with("+ ")
                    || trimmed.starts_with("- [")
                {
                    prev.chars().take_while(|c| c.is_whitespace() && *c != '\n').collect()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            let current_line_blank = b.lines[line_idx].trim().is_empty();
            buffer::push_undo(b);
            let insert = if current_line_blank && col == 0 {
                format!("{prev_indent}{marker}")
            } else {
                format!("\n{prev_indent}{marker}")
            };
            // Route through the multi-line insert primitive so a leading
            // newline splits into a new line entry rather than embedding a raw
            // '\n' inside one line's storage. Columns here are 1-based.
            buffer::apply_insert_internal(
                &mut b.lines,
                &mut b.selections,
                line_idx + 1,
                col + 1,
                &insert,
            );
            // Move cursor to end of inserted marker.
            let new_col = col + insert.chars().count();
            let new_line = line_idx + insert.matches('\n').count() + 1;
            // For multi-line insertion (newline included), the cursor lands
            // on the new last line at the marker end.
            let final_line = if insert.starts_with('\n') {
                new_line
            } else {
                line_idx + 1
            };
            let final_col = if insert.starts_with('\n') {
                prev_indent.chars().count() + marker.chars().count() + 1
            } else {
                new_col + 1
            };
            b.selections = vec![final_line, final_col, final_line, final_col];
            b.change_id += 1;
            Ok(())
        });
    }
}
"doc:reload" => {
    if let Some(doc) = docs.get_mut(active_tab) {
        if !doc.path.is_empty() {
            if doc_is_modified(doc) {
                nag = Nag::ReloadFromDisk { path: doc.path.clone() };
            } else if let Some(buf_id) = doc.view.buffer_id {
                let path = doc.path.clone();
                let _ = buffer::with_buffer_mut(buf_id, |b| {
                    let mut fresh = buffer::default_buffer_state();
                    // Only replace the buffer when the reload actually read the
                    // file; on error keep the current contents so a transient
                    // read failure cannot wipe an unmodified document.
                    if buffer::load_file(&mut fresh, &path).is_ok() {
                        b.lines = fresh.lines;
                        b.change_id = b.change_id.wrapping_add(1).max(1);
                    }
                    Ok(())
                });
                if let Ok((cid, sig)) = buffer::with_buffer(buf_id, |b| {
                    Ok((b.change_id, buffer::content_signature(&b.lines)))
                }) {
                    doc.saved_change_id = cid;
                    doc.saved_signature = sig;
                }
                doc.cached_change_id = -1;
                doc.cached_render = std::sync::Arc::new(Vec::new());
            }
        }
    }
}
"git:pull" | "git:push" | "git:commit" | "git:stash" => {
    if subsystems.has_git() {
        // pull/push reach the network and can stall on an unreachable remote, so
        // run every mutation on a git worker thread. main_loop picks up the result
        // each frame via git::drain_finished_mutations and refreshes the status.
        let (label, git_cmd): (&str, Vec<String>) = match cmd.as_str() {
            "git:pull" => ("git pull", vec!["pull".into()]),
            "git:push" => ("git push", vec!["push".into()]),
            "git:commit" => (
                "git commit",
                vec![
                    "commit".into(),
                    "--allow-empty-message".into(),
                    "-m".into(),
                    String::new(),
                ],
            ),
            "git:stash" => ("git stash", vec!["stash".into()]),
            _ => ("", Vec::new()),
        };
        if !git_cmd.is_empty() {
            crate::editor::git::start_mutation(&project_root, label, &git_cmd);
        }
    }
}
"lsp:hover" => {
    if subsystems.has_lsp() {
        if let Some(doc) = docs.get(active_tab) {
            if let Some(buf_id) = doc.view.buffer_id {
                if let Some(tid) = lsp_state.transport_id {
                    if lsp_state.initialized && !doc.path.is_empty() {
                        let (cl, cc) = buffer::with_buffer(buf_id, |b| {
                            Ok((*b.selections.get(2).unwrap_or(&1), *b.selections.get(3).unwrap_or(&1)))
                        }).unwrap_or((1, 1));
                        let uri = path_to_uri(&doc.path);
                        let req_id = lsp_state.next_id();
                        lsp_state.pending_requests.insert(req_id, "textDocument/hover".to_string());
                        let _ = lsp::send_message(tid, &lsp_hover_request(req_id, &uri, cl - 1, cc - 1));
                        hover.line = cl;
                        hover.col = cc;
                    }
                }
            }
        }
    }
}
"lsp:go-to-definition" => {
    if subsystems.has_lsp() {
        if let Some(doc) = docs.get(active_tab) {
            if let Some(buf_id) = doc.view.buffer_id {
                if let Some(tid) = lsp_state.transport_id {
                    if lsp_state.initialized && !doc.path.is_empty() {
                        let (cl, cc) = buffer::with_buffer(buf_id, |b| {
                            Ok((*b.selections.get(2).unwrap_or(&1), *b.selections.get(3).unwrap_or(&1)))
                        }).unwrap_or((1, 1));
                        let uri = path_to_uri(&doc.path);
                        let req_id = lsp_state.next_id();
                        lsp_state.pending_requests.insert(req_id, "textDocument/definition".to_string());
                        let _ = lsp::send_message(tid, &lsp_definition_request(req_id, &uri, cl - 1, cc - 1));
                    }
                }
            }
        }
    }
}
"lsp:go-to-implementation" | "lsp:go-to-type-definition" | "lsp:find-references" => {
    if subsystems.has_lsp() {
        let method = match cmd.as_str() {
            "lsp:go-to-implementation" => "textDocument/implementation",
            "lsp:go-to-type-definition" => "textDocument/typeDefinition",
            "lsp:find-references" => "textDocument/references",
            _ => unreachable!(),
        };
        if let Some(doc) = docs.get(active_tab) {
            if let Some(buf_id) = doc.view.buffer_id {
                if let Some(tid) = lsp_state.transport_id {
                    if lsp_state.initialized && !doc.path.is_empty() {
                        let (cl, cc) = buffer::with_buffer(buf_id, |b| {
                            Ok((*b.selections.get(2).unwrap_or(&1), *b.selections.get(3).unwrap_or(&1)))
                        }).unwrap_or((1, 1));
                        let uri = path_to_uri(&doc.path);
                        let req_id = lsp_state.next_id();
                        lsp_state.pending_requests.insert(req_id, method.to_string());
                        let _ = lsp::send_message(tid, &lsp_position_request(req_id, method, &uri, cl - 1, cc - 1));
                    }
                }
            }
        }
    }
}
"doc:format" => {
    if subsystems.has_lsp()
        && lsp_state.initialized
        && let Some(doc) = docs.get(active_tab)
        && let Some(tid) = lsp_state.transport_id
        && !doc.path.is_empty()
    {
        let ext = doc.path.rsplit('.').next().unwrap_or("");
        let is_lsp = ext_to_lsp_filetype(ext)
            .map(|ft| ft == lsp_state.filetype)
            .unwrap_or(false);
        if is_lsp {
            let uri = path_to_uri(&doc.path);
            let tab_size = doc.indent_size.max(1);
            let insert_spaces = doc.indent_type != "hard";
            let req_id = lsp_state.next_id();
            lsp_state
                .pending_requests
                .insert(req_id, "textDocument/formatting".to_string());
            lsp_state.pending_request_uris.insert(req_id, uri.clone());
            let _ = lsp::send_message(
                tid,
                &lsp_formatting_request(req_id, &uri, tab_size, insert_spaces),
            );
        }
    }
}
"lsp:rename" => {
    if subsystems.has_lsp()
        && lsp_state.initialized
        && let Some(doc) = docs.get(active_tab)
        && let Some(buf_id) = doc.view.buffer_id
        && lsp_state.transport_id.is_some()
        && !doc.path.is_empty()
    {
        let ext = doc.path.rsplit('.').next().unwrap_or("");
        let is_lsp = ext_to_lsp_filetype(ext)
            .map(|ft| ft == lsp_state.filetype)
            .unwrap_or(false);
        if is_lsp {
            let (cl, cc, word) = buffer::with_buffer(buf_id, |b| {
                let line = *b.selections.get(2).unwrap_or(&1);
                let col = *b.selections.get(3).unwrap_or(&1);
                let word = crate::editor::picker::word_at(&b.lines, line, col);
                Ok((line, col, word))
            })
            .unwrap_or((1, 1, String::new()));
            lsp_rename_pos = Some((path_to_uri(&doc.path), cl - 1, cc - 1));
            cmdview_active = true;
            cmdview_mode = CmdViewMode::LspRename;
            cmdview_label = "Rename to:".to_string();
            cmdview_text = word;
            cmdview_cursor = cmdview_text.len();
            cmdview_suggestions.clear();
            cmdview_selected = 0;
            completion.hide();
        }
    }
}
"lsp:code-action" => {
    if subsystems.has_lsp()
        && lsp_state.initialized
        && let Some(doc) = docs.get(active_tab)
        && let Some(buf_id) = doc.view.buffer_id
        && let Some(tid) = lsp_state.transport_id
        && !doc.path.is_empty()
    {
        let ext = doc.path.rsplit('.').next().unwrap_or("");
        let is_lsp = ext_to_lsp_filetype(ext)
            .map(|ft| ft == lsp_state.filetype)
            .unwrap_or(false);
        if is_lsp {
            let uri = path_to_uri(&doc.path);
            let (l1, c1, l2, c2) = buffer::with_buffer(buf_id, |b| {
                Ok((
                    *b.selections.first().unwrap_or(&1),
                    *b.selections.get(1).unwrap_or(&1),
                    *b.selections.get(2).unwrap_or(&1),
                    *b.selections.get(3).unwrap_or(&1),
                ))
            })
            .unwrap_or((1, 1, 1, 1));
            // Diagnostics overlapping the cursor line give the server context
            // for quick fixes.
            let diags: Vec<serde_json::Value> = lsp_state
                .diagnostics
                .get(&uri)
                .map(|ds| {
                    ds.iter()
                        .filter(|d| d.start_line < l2 && d.end_line >= l1.saturating_sub(1))
                        .map(|d| {
                            serde_json::json!({
                                "range": {
                                    "start": {"line": d.start_line, "character": d.start_col},
                                    "end": {"line": d.end_line, "character": d.end_col}
                                },
                                "severity": d.severity,
                                "message": d.message
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let req_id = lsp_state.next_id();
            lsp_state
                .pending_requests
                .insert(req_id, "textDocument/codeAction".to_string());
            let _ = lsp::send_message(
                tid,
                &lsp_code_action_request(
                    req_id,
                    &uri,
                    l1 - 1,
                    c1 - 1,
                    l2 - 1,
                    c2 - 1,
                    serde_json::Value::Array(diags),
                ),
            );
        }
    }
}
"scale:increase" | "scale:decrease" | "scale:reset" => {
    // Handled by direct key check above the dispatch.
}
_ => {
    // Default: forward to handle_doc_command and bump LSP edit tracking.
    // Keyboard-initiated: auto-scroll to keep cursor visible.
    if let Some(doc) = docs.get_mut(active_tab) {
        let marker = comment_marker_for_path(&doc.path, &syntax_index);
        handle_doc_command(
            &mut doc.view,
            &cmd,
            &style,
            &doc.indent_type,
            doc.indent_size,
            marker.as_ref(),
            true,
            line_wrapping,
        );
    }
    let is_edit_cmd = matches!(cmd.as_str(),
        "doc:newline" | "doc:newline-below" | "doc:newline-above"
        | "doc:backspace" | "doc:delete"
        | "doc:delete-to-previous-word-start" | "doc:delete-to-next-word-end"
        | "doc:indent" | "doc:unindent"
        | "doc:toggle-line-comments"
        | "doc:move-lines-up" | "doc:move-lines-down"
        | "doc:duplicate-lines" | "doc:delete-lines"
        | "doc:join-lines"
        | "core:sort-lines" | "doc:fold" | "doc:unfold" | "doc:unfold-all"
    );
    if is_edit_cmd && lsp_state.transport_id.is_some() && lsp_state.initialized {
        lsp_state.inlay_hints.clear();
        if let Some(doc) = docs.get(active_tab) {
            if !doc.path.is_empty() {
                lsp_state.last_change = Some(Instant::now());
                lsp_state.pending_change_uri = Some(path_to_uri(&doc.path));
                lsp_state.pending_change_version += 1;
            }
        }
    }
}
}
}
}
