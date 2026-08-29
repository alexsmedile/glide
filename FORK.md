# Fork notes

This fork adds resize, layout, and focus behaviour that AeroSpace had and
upstream Glide does not. Existing configs keep working.

The config that drives it lives in its own repo at
`~/code/utils/glide-config`, and requires this fork — upstream rejects it with
a parse error.

`cargo test --lib` on `main` is 274 tests. Each feature's tests were checked to
fail with the implementation removed, so they test the behaviour rather than
passing either way.

## Fork releases

Fork releases use `fork-v<upstream>-r<N>`. The upstream version records the
Glide release this fork is based on; the revision increments for each stable
fork checkpoint and resets when the upstream version changes. The same tag is
applied to the matching `glide-config` commit.

The current identifier is stored in `FORK_VERSION`. Fork tags deliberately do
not begin with `v`, so they cannot trigger upstream's `v*.*.*` packaging
workflow.

| Release | Upstream base | Contents |
|---|---|---|
| `fork-v0.2.15-r4` | `v0.2.15` | Fullscreen gap handling for single-window layouts. |
| `fork-v0.2.15-r3` | `v0.2.15` | Configurable gapless fullscreen and automatic gapless maximization for single-window layouts. |
| `fork-v0.2.15-r2` | `v0.2.15` | Mouse drop overlays and layout placement, keyboard half-screen snapping, floating/unmanaged size presets, group navigation, and Alt-scroll group cycling. |
| `fork-v0.2.15-r1` | `v0.2.15` | Five layout/resize features plus reliable per-Space focus restoration on multiple displays. |

## Branches

| Branch | Contents |
|---|---|
| `main` | All features, stacked. This is the branch to build and run. |
| `upstream-main` | Clean upstream, no local commits. Rebase target. |
| `feat/*` | One feature each, branched off `upstream-main`, for PRs. |

Each `feat/*` branch builds and passes tests on its own, so it can be
reviewed without the others. `main` is those same commits in order, so it
stays a fast-forward of whatever upstream takes.

## Upstream status

| Feature | Branch | Upstream |
|---|---|---|
| `toggle_orientation` | `feat/toggle-orientation` | PR #237 |
| `balance` | `feat/balance-sizes` | PR #238 |
| smart `resize` | `feat/resize-smart` | PR #239 |
| `default_root_orientation` | `feat/auto-root-orientation` | PR #242, issue #241 |
| `set_proportion` | `feat/resize-presets` | issue #240, no PR yet |
| Space focus memory | integrated on `main` | local validation complete |
| Mouse layout placement | `feat/layout-drag-overlays` | local validation complete |

`set_proportion` is deliberately an issue rather than a PR: upstream already
has `cycle_column_width`, which reads a presets list from config, so whether
this should be a per-binding argument or a cycling command is a question for
the maintainer before the code is written.

## Features

### Smart fullscreen gaps

`fullscreen_uses_outer_gap` controls whether Glide's layout fullscreen command
keeps the configured outer gap. `single_window_uses_outer_gap` controls whether
a layout containing one tiled window keeps it. Both default to `true` for
upstream-compatible behaviour and can be disabled independently.

These settings maximize a window inside the macOS Space's usable screen. They
do not invoke macOS native fullscreen or create a separate fullscreen Space.

### Mouse placement and snap overlays

Dragging to a screen edge shows blue Rectangle-style previews for halves and
quadrants. Releasing applies the preview as a floating frame. Holding Alt while
dragging over a tiled window adds layout-aware targets: purple edges split the
tree beside that window and the green center groups the windows into a stack.

Corner reach, split reach, and edge activation are configurable with
`mouse_drag_corner_size`, `mouse_drag_split_ratio`, and
`mouse_drag_snap_distance`.

### Keyboard snapping and smart size presets

The `snap_window` command places the focused window in a screen half and works
on managed and unmanaged Spaces. On a managed Space it deliberately floats the
window; `toggle_window_floating` attaches it to the layout again.

`set_proportion` remains layout-relative for tiled windows. For floating
windows and unmanaged Spaces, it instead resizes the window width to that
fraction of the screen while preserving its position and height.

### Group navigation

`focus_group_next` and `focus_group_prev` cycle only within the nearest tabbed
or stacked group. Holding Alt and scrolling over the visible group window
provides the same navigation with the mouse, with gesture throttling for
trackpads.

### Space focus memory

Restores the remembered window when returning to a managed Space instead of
leaving focus on another display. Space changes expose only the displays whose
managed Space actually changed, preventing an unchanged display from winning
the focus/raise race. Click-to-switch behaviour is preserved when macOS has
already focused a window in the destination Space.

### `toggle_orientation` — `feat/toggle-orientation`

Flips the parent container between horizontal and vertical, rearranging the
windows already inside it. Groups stay groups: tabbed flips to stacked.

Upstream's `split` only declares an orientation for windows opened *later*;
there was no way to reorient a container that already has windows in it.

```toml
"Alt + Slash" = "toggle_orientation"
```

### `balance` — `feat/balance-sizes`

Resets every container on the space to divide its space evenly, nested ones
included. Sizing is a weight per node, so this sets every weight to 1.

```toml
"Alt + Shift + Digit0" = "balance"
```

### `set_proportion` — `feat/resize-presets`

Gives the focused window an exact share of its container, along whichever
axis the container uses. Absolute rather than incremental, so a key bound to
a fraction always lands on that fraction and pressing it twice is a no-op.

```toml
"Alt + Ctrl + Digit1" = { set_proportion = { proportion = 0.333 } }
"Alt + Ctrl + Digit5" = { set_proportion = { proportion = 0.5 } }
```

For a tiled window, the proportion is of the parent container, not the screen:
a window nested inside a half-screen split set to `0.5` fills a quarter of the
screen. For a floating window or unmanaged Space, it is a fraction of the
screen width.

### `default_root_orientation` — `feat/auto-root-orientation`

Splits the screen's longer axis when set to `auto`, so windows stack on a
portrait screen rather than becoming tall narrow strips.

```toml
default_root_orientation = "auto"
```

Applies when a layout is created. A layout that moves to a differently
shaped screen keeps the orientation it has, rather than being rearranged
underneath you. Scroll layouts are unaffected.

### Smart `resize` — `feat/resize-smart`

Omitting `direction` resizes along whichever axis the container uses, so one
key pair grows and shrinks in both a row and a column. A negative percent
shrinks. `direction` is now optional, so existing configs are unaffected.

Smart means it picks the *axis*, not the *side*. The space comes from the
next window, or the previous one when there is no next, so a window in the
middle of a row always moves its right edge:

```
before   [300][300][300]
grow     [300][390][210]     right edge moved; left window untouched
```

Taking from whichever neighbour has more room was the alternative. It is
rejected on purpose: which neighbour is larger changes as the layout
changes, so repeated presses would move a different edge each time.

Within a nested tree it acts on the container the window belongs to, so a
window in a vertical stack grows downward rather than sideways. Groups do
nothing, since only one window shows at a time.

```toml
"Alt + Minus" = { resize = { percent = -5 } }
"Alt + Shift + Equal" = { resize = { percent = 5 } }
```

## The edge case worth knowing

A node at the *end* of its container has no neighbour past its far edge, so
resizing toward that edge finds no one to trade space with and does nothing.
This is why a resize key can look dead on the rightmost window in a row while
working everywhere else — it affects upstream's own `Alt + Ctrl + L` too.

Both `set_proportion` and smart `resize` fall back to the neighbour on the
near side. The fixed-direction `resize` still behaves as upstream does.

## Building

```
cargo build --release
cargo test
```

The live fork uses `/Applications/Glide-Dev.app`, with bundle identifier
`org.glidewm.glide-dev`, signed using the same Developer ID identity on every
build. Do not ad-hoc re-sign the bundle: changing its designated requirement
can invalidate the macOS Accessibility grant. Keep stock `Glide.app` installed
as a signed fallback.
