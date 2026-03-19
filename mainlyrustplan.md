# Mainly Rust Plan For Lite-Anvil

## Objective

Move Lite-Anvil to a Rust-owned editor runtime that reaches at least 75% Rust overall while explicitly retaining Lua as the plugin and lightweight asset layer.

This is not the same as the existing 100% Rust plan.

The target end state here is:

- Rust owns startup, runtime state, editor shell, document/view lifecycle, commands, session, config, and all built-in product-defining features.
- Lua remains available for plugins, optional automation, and lightweight declarative/editor-extension assets.
- Built-in features that feel like core editor/product behavior are no longer shipped as Lua plugins.
- The remaining Lua surface is intentionally narrow and plugin-oriented.

## Baseline In This Checkout

Raw `.rs` / `.lua` LOC scan in this checkout:

- Rust: 14,829 LOC
- Lua: 34,229 LOC
- Overall Rust share: about 30.2%

I am using your stated 32% Rust / 68% Lua as the working product baseline, but the local file count lands slightly lower.

Important current Lua concentrations:

- `data/core/*.lua`: 15,819 LOC
- feature plugins in `data/plugins` excluding `language_*.lua`: 12,866 LOC
- language plugins in `data/plugins/language_*.lua`: 3,050 LOC
- color themes in `data/colors/*.lua`: 277 LOC

This is the main takeaway: the fastest path to 75%+ Rust is not deleting Lua plugins in general. It is moving the editor shell and the built-in feature plugins into Rust.

## Product Decision

Retain Lua, but narrow its role.

Lua should continue to exist for:

- third-party plugins
- user automation/macros
- lightweight feature experiments
- declarative syntax/theme assets until there is a better asset format

Lua should stop owning:

- startup and package bootstrapping
- document lifecycle and pane shell orchestration
- built-in navigation/search/editor UX that defines the core product
- built-in terminal, Git, LSP, autocomplete, project tree, session/workspace management
- editor-shell configuration logic

## ecode Boundary To Copy

The relevant lesson from the `eepp` / `ecode` reference is architectural, not language-specific:

- editor shell is a first-class core subsystem: `include/eepp/ui/uicodeeditor.hpp`
- split/editor orchestration is core UI infrastructure: `include/eepp/ui/tools/uicodeeditorsplitter.hpp`
- terminal management is application-owned: `src/tools/ecode/terminalmanager.hpp`
- status-bar terminal integration is application-owned: `src/tools/ecode/statusterminalcontroller.hpp`
- LSP document integration is application-owned: `src/tools/ecode/plugins/lsp/lspdocumentclient.hpp`
- folding is tied into document/editor infrastructure, not a loose script feature

That boundary maps cleanly onto Lite-Anvil. Anything that shapes the editor shell, split management, document presentation, code intelligence, or integrated tool panes should move into the main Rust application.

## Target Distribution

### Recommended Landing Zone

- overall repo: 78% to 82% Rust
- runtime/editor core: 88% to 93% Rust
- Lua kept mainly for plugin-facing code and syntax/theme assets

### Why This Is Realistic

If Lite-Anvil moves:

- all of `data/core` into Rust
- the major built-in feature plugins into Rust

then more than 28k lines of current Lua ownership shift to Rust-owned modules. Even allowing for rewrite churn and some Lua to remain in plugin land, 75%+ Rust is realistic without killing Lua support.

## What Should Move Into The Main Application

These should become built-in Rust modules and stop shipping as Lua plugins.

### Tier 1: Must Move

These are core product features, and ecode treats comparable behavior as part of the application/editor shell.

| Area | Current Lua | Why It Should Be Rust |
|---|---|---|
| Startup and runtime bootstrap | `data/core/start.lua`, `data/core/init.lua` | App ownership, restart/session, config, plugin loading, event loop coordination should not live in Lua. |
| Document model and editing shell | `data/core/doc/*`, `data/core/docview.lua`, `data/core/rootview.lua`, `data/core/node.lua`, `data/core/statusview.lua`, `data/core/commandview.lua` | This is the editor. Rust should own state, rendering decisions, pane graph, and command routing. |
| Tree view | `data/plugins/treeview.lua` | Project tree is core navigation. Already partly backed by `tree_model.rs`. |
| File finder / picker flows | `data/plugins/findfile.lua`, command palette wiring in core | This is part of the main shell, not an optional script. |
| Project search / replace | `data/plugins/projectsearch.lua`, `data/plugins/projectreplace.lua` | Search engine is already Rust-backed. UX and command flow should join it in Rust. |
| Autocomplete | `data/plugins/autocomplete/*` | Completion UX is central editor behavior. |
| LSP | `data/plugins/lsp/*` | LSP transport/state is already in Rust. UI and doc integration should move too. |
| Terminal | `data/plugins/terminal/*` | ecode treats terminal management as application-owned. Lite-Anvil should too. |
| Git integration | `data/plugins/git/*` | Native Git model already exists. Status UI should become built-in Rust. |
| Workspace/session persistence | `data/plugins/workspace.lua` plus session logic in `data/core/init.lua` | Persistence is shell ownership. |
| Toolbar / shell affordances | `data/plugins/toolbarview.lua` | If shipped by default, it is app chrome and should be Rust. |

### Tier 2: Should Move

These are smaller, but they are editor affordances that materially affect the feel of the product. ecode-style editors usually expose these as built-in editor behavior, not optional scripts.

| Area | Current Lua | Recommendation |
|---|---|---|
| Folding | `data/plugins/folding.lua` | Move to Rust and integrate with document/view model. |
| Bracket matching | `data/plugins/bracketmatch.lua` | Move to Rust as a native editor overlay. |
| Line wrapping | `data/plugins/linewrapping.lua` | Move to Rust because it affects layout and viewport math. |
| Line guide | `data/plugins/lineguide.lua` | Move to Rust as a rendering/config toggle. |
| Detect indent | `data/plugins/detectindent.lua` | Move to Rust so document open/save heuristics live with the document model. |
| Draw whitespace | `data/plugins/drawwhitespace.lua` | Move to Rust as a display toggle. |
| Trim whitespace | `data/plugins/trimwhitespace.lua` | Move to Rust save pipeline. |
| Theme toggle | `data/plugins/theme_toggle.lua` | Replace with Rust-owned settings/UI action. |
| Scale | `data/plugins/scale.lua` | Fold into Rust config/startup since scaling is platform/runtime behavior. |
| Autoreload / autorestart | `data/plugins/autoreload.lua`, `data/plugins/autorestart.lua` | Runtime/process ownership belongs in Rust. |
| Markdown preview | `data/plugins/markdown_preview/*` | Should be Rust if kept as a bundled feature, since native markdown support already exists. |

### Tier 3: Can Stay Lua

These are the plugins I would explicitly keep as Lua in the mainly-Rust plan.

| Area | Current Lua | Why It Can Stay Lua |
|---|---|---|
| Language definitions | `data/plugins/language_*.lua` | Good fit for asset/plugin layer. They are numerous, change often, and are not core shell behavior. |
| Small editing helpers | `tabularize.lua`, `quote.lua`, `reflow.lua` | Useful, but not product-defining. |
| Automation/macro features | `macro.lua` | Strong fit for scripting. |
| Remote/experimental workflows | `remotessh.lua` | Higher churn and less central than tree/LSP/search/terminal. |
| Themes as data | `data/colors/*.lua` initially | Fine to keep until a declarative theme format replaces Lua. |

## What This Means For Built-In Plugins

The built-in plugin set should be split into two categories:

### Built-In Features That Stop Being Plugins

- tree view
- file finder / command palette shell integration
- project search and replace
- autocomplete
- LSP
- terminal
- Git
- workspace/session persistence
- toolbar and status integrations
- folding
- bracket matching
- line wrapping
- line guide
- detect indent
- whitespace rendering and trimming
- markdown preview if it remains bundled

These become Rust modules in the main app and should be 100% Rust.

### Plugins That Remain Lua-Backed

- language syntax bundles
- user-installed plugins
- small automation helpers
- optional experimental features

## Architecture Target

### 1. Rust Owns Runtime First

Move Lite-Anvil to:

- Rust app startup
- Rust config load/merge
- Rust session restore/save
- Rust command registry
- Rust pane/tab/split graph
- Rust document/view lifecycle
- Rust event dispatch
- Rust built-in module registry

Lua then loads after the app is already alive, through a Rust-owned plugin host.

### 2. Keep Lua As A Hosted Extension Layer

Retain:

- embedded Lua VM
- plugin loading API
- plugin lifecycle hooks
- command registration from plugins
- lightweight overlays and automation hooks

Do not retain:

- Lua ownership of the main event loop
- Lua ownership of built-in editor shell features
- Lua ownership of global state that Rust must reason about for performance or correctness

### 3. Split “Core” From “Extension API”

Define two explicit surfaces:

- `forge-core` internal Rust APIs for built-in modules
- a smaller stable Lua plugin API for extensions

That avoids today’s blurred boundary where Lua is both the shell and the extension system.

## Detailed Migration Phases

## Phase 0: Inventory And Lock The Boundary

Deliverables:

- feature inventory for all built-in Lua plugins
- ownership map: `Rust core`, `Rust built-in module`, `Lua plugin`, `Lua asset`
- current command inventory and keybinding inventory
- config inventory covering core and all bundled plugins
- parity checklist for each Tier 1 and Tier 2 feature

Exit criteria:

- every bundled Lua file has a target bucket
- all Tier 1 modules have identified Rust homes

## Phase 1: Rust Shell Skeleton

Move these into Rust first:

- startup path now in `data/core/start.lua`
- top-level app/session logic now in `data/core/init.lua`
- config loading and derived defaults
- session persistence
- command registry
- plugin manager bootstrap

Keep Lua plugins working through compatibility shims while Rust becomes the owner of startup and app state.

Exit criteria:

- app starts without `data/core/start.lua`
- Rust creates the runtime state before Lua plugin load
- Lua no longer owns restart/session/config bootstrap

## Phase 2: Rust Document And View Core

Port the main editor shell from `data/core`:

- document lifecycle from `data/core/doc/init.lua`
- root pane graph from `data/core/rootview.lua` and `data/core/node.lua`
- editor/status/command views from `data/core/docview.lua`, `data/core/statusview.lua`, `data/core/commandview.lua`
- keymap and command dispatch from `data/core/keymap.lua`, `data/core/command.lua`, `data/core/commands/*`

This phase is the real pivot. Without it, Lite-Anvil is still Lua-led even if more backend APIs are native.

Exit criteria:

- pane/tab/split state is Rust-owned
- documents are Rust-owned end-to-end
- command dispatch is Rust-owned
- Lua plugins talk to Rust-owned views/docs via API handles

## Phase 3: Move The Big Built-In Feature Plugins

Port these as Rust modules in this order:

1. tree view
2. file finder / picker shell flow
3. project search / replace
4. autocomplete
5. terminal
6. Git
7. workspace/session
8. toolbar shell
9. markdown preview
10. LSP UX layer

Recommended module grouping:

- `shell/tree`
- `shell/picker`
- `shell/search`
- `shell/terminal`
- `shell/git`
- `shell/workspace`
- `editor/completion`
- `editor/lsp`
- `editor/markdown`

Exit criteria:

- all Tier 1 built-ins are loaded as Rust modules
- bundled distribution no longer depends on those Lua plugins

## Phase 4: Move The Editor Affordance Plugins

Port:

- folding
- bracket matching
- line wrapping
- line guide
- detect indent
- draw whitespace
- trim whitespace
- theme toggle
- scale
- autoreload
- autorestart

These should be implemented as:

- document services
- view overlays
- save/open hooks
- settings toggles

not as free-floating scripts.

Exit criteria:

- Tier 2 plugins are either gone or reduced to thin Lua wrappers calling native behavior

## Phase 5: Narrow The Lua Plugin API

After the built-ins are out of Lua, simplify the remaining Lua API around extension use cases:

- commands
- editor/document hooks
- overlays
- menu/status additions
- file/project automation
- syntax/theme registration

Remove or hide shell-internal APIs that were only needed because Lua used to run the app.

Exit criteria:

- Lua API is smaller and more stable
- Rust internals are no longer exposed accidentally

## Phase 6: Optional Asset Cleanup

Only after the architecture stabilizes:

- move language syntax definitions from Lua code to declarative asset files if desired
- move themes from Lua to TOML/JSON/ron if desired
- keep compatibility loaders if low cost

This phase is optional for the 75%+ goal.

## Config Strategy

Use Rust-owned config for built-ins and keep plugin config namespaced.

Recommended split:

- `config.toml` for editor, shell, search, Git, terminal, LSP, display, session, workspace
- `plugins.toml` for Lua plugin settings
- `keymaps.toml` for user bindings

Rules:

- built-in Rust features no longer read `config.plugins.*` as their source of truth
- Lua plugins still may use `config.plugins.*` or a Rust-provided equivalent
- settings that affect startup, layout, rendering, session, or core commands must become Rust-owned

## Testing And Parity Gates

For each Tier 1 module:

- behavior checklist
- command/keybinding parity
- persistence parity where applicable
- performance sanity check on large projects/files
- crash/restart/session tests

Add focused fixtures for:

- split and tab persistence
- search/replace results and replace previews
- terminal reuse/open-position modes
- Git status refresh and large repo behavior
- LSP diagnostics, completion, rename, references, semantic tokens, folding
- tree expansion, selection, reveal, hidden/ignored file toggles

## Risks

### Biggest Risk

Trying to preserve the current Lua-owned shell while also migrating built-ins one by one to Rust will leave Lite-Anvil in an awkward middle state for too long.

The right move is to port the shell first, then the heavy built-ins.

### Secondary Risks

- command/keybinding regressions during shell migration
- plugin API breakage if shell internals leak into extension APIs
- duplicate settings paths during transition
- temporary UX mismatch between Rust-owned and Lua-owned panels

## Recommended Sequencing

If the goal is 75%+ Rust with the best return on effort, do this:

1. Rust startup/config/session/plugin bootstrap
2. Rust pane/tab/document shell
3. Rust tree + picker + project search/replace
4. Rust terminal + Git + workspace
5. Rust autocomplete + LSP UX
6. Rust editor affordances like folding/bracket matching/wrapping
7. shrink and stabilize Lua plugin API

## Likely Final Distribution

Assuming:

- all `data/core` behavior moves to Rust
- Tier 1 and most Tier 2 built-ins move to Rust
- language plugins and a small optional plugin set remain Lua

the likely result is:

- Rust: about 78% to 82% of the repo
- Lua: about 18% to 22% of the repo

And more importantly:

- built-in runtime behavior: roughly 90% Rust
- Lua role: plugins, syntax bundles, themes, automation

## Bottom Line

The most effective plan is not “keep most things as Lua plugins and gradually add Rust helpers.”

The effective plan is:

- Rust becomes the editor application.
- Lua remains the extension layer.
- bundled features that define the editor experience stop pretending to be plugins.

That reaches the 75%+ Rust target while still preserving Lua where Lua actually adds value.
