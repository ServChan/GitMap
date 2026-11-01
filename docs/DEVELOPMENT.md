# Development

Everything is Rust: the window uses [winit](https://github.com/rust-windowing/winit), graphics use [Vello](https://github.com/linebender/vello) (a 2D renderer on wgpu compute shaders), text uses [Parley](https://github.com/linebender/parley), and history comes from [gitoxide](https://github.com/GitoxideLabs/gitoxide). Interface elements and animation are written here, in `src/ui.rs` and `src/anim.rs`.

## Code Structure

| File | What it does |
|---|---|
| `src/scan.rs` | finds repositories, skipping builds and dependencies |
| `src/git.rs` | all git reads: history, project state, line comparison |
| `src/watch.rs` | watches the open repository |
| `src/overview.rs`, `src/diff.rs` | folder overview, comparison sheet |
| `src/actions.rs`, `src/export.rs` | external tools, SVG export |
| `src/layout.rs` | map layout, pure logic with tests |
| `src/map.rs` | camera, input, drawing and animation of the map |
| `src/ui.rs` | custom interface elements: buttons, fields, scrolling, focus |
| `src/anim.rs` | springs and curves |
| `src/text.rs` | text layout and glyph drawing |
| `src/gfx.rs` | GPU output and PNG frame capture |
| `src/shell.rs` | frameless window, events, frame rate |
| `src/app.rs`, `src/index.rs`, `src/panel.rs` | state, project list, right panel |
| `src/install.rs`, `src/installer.rs`, `src/update.rs` | installation, wizard, updates |
| `src/headless.rs` | debug build only: runs a script with no window |

Layout takes four steps: commits are split into lines along first-parent chains, lines are placed in columns by both ancestry and time, lines get lanes with no overlaps, then columns are spread so that transitions stay at 45°.

While nothing moves, no frames are drawn. Trains and the HEAD signal run at 30 frames per second, and only while the window has focus.

## Headless Checks

The debug build can play a script and save frames without showing anything on screen. Time advances in steps of 1/60 s, so animation can be inspected frame by frame:

```bash
cargo run -- E:\Coding\Minecraft\Iris --script "wait;run 0.4;shot a.png;key Home;run 1.5;shot b.png"
```

Commands: `size w h`, `wait`, `run seconds`, `sleep ms`, `shot file`, `move x y`, `click x y`, `wheel x y delta`, `key Name`, `text string`, `exec command`, `export png|svg file`, `wizard install`, `wizard uninstall`.

## Releasing

The signing key is created once with `scripts\release\new-updater-key.ps1`. The private key is stored in Credential Manager as `gitmap-release:updater-key`, and the public key is in `assets\updater.pub` and built into the app. Losing the private key leaves installed copies without updates, so keep a backup of it.

Raise the version in `Cargo.toml`, then run:

```bash
scripts\release\publish.ps1 -Notes "What changed"
```

The script builds `dist\GitMap-Setup.exe` and a signed `dist\latest.json`. With `-Publish` it uploads the release to `ServChan/gitmap-releases`, which must stay public.
