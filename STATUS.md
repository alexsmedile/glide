# Status — Glide Fork

**Last updated:** 2026-09-16
**Current objective:** Evolve the stable private fork into a hybrid tiling and snapping window manager for keyboard and mouse power users.
**Overall state:** `fork-v0.2.15-r7` stable checkpoint · Worksets opt-in behind a config flag · display-aware desktops for bindings, window rules, and managed spaces · 314 automated tests passing

---

## 1. Verified Completed Outputs

- `src/actor/reactor.rs` and `src/actor/layout.rs`: reliable remembered focus on multi-display Space changes, mouse drop handling, floating restoration, screen snapping, and group navigation.
- `src/model/layout_tree.rs` and `src/model/size.rs`: tree/group manipulation, exact proportions, balancing, automatic root orientation, and configurable gapless fullscreen/single-window layouts.
- `src/actor/drop_preview.rs` and `src/ui/group_bar.rs`: visual drop previews and indicators for stacked/tabbed groups.
- `glide.default.toml`: upstream-compatible defaults for fork commands and settings.
- `FORK.md` and `CHANGELOG.md`: fork behavior, release history, and versioning documented through `fork-v0.2.15-r7`.
- `TODO.md`: make-a-change roadmap prioritizing Worksets, precision placement, a scriptable API, Recipes, and pinned windows.
- `WORKSETS.md`: canonical Space, Workset, Layout, Window, Role, Slot, Recipe, pinning, and Stage Manager ontology.
- Named Worksets: per-Space layouts, direct selection, cycling, rule-based routing, focus restoration, inactive-window suppression, and HUD feedback.
- Native Space handoff: macOS exclusively owns Alt+digits; direct Workset shortcuts can request the native Desktop transition and activate after confirmation.
- Runtime hardening: zero WindowServer ids no longer panic, and transient non-resizable `AXUnknown` surfaces no longer disturb tiled layouts.
- Workset layering: active windows are raised sequentially across applications, remembered focus is restored last, and an incomplete visible stack is verified and retried without hiding or moving inactive windows.

## 2. Active Decisions & Constraints

- **Architecture:** Preserve the actor → model → sys dependency direction; model code remains deterministic and side-effect free.
- **Compatibility:** New configuration settings default to upstream behavior; personal behavior is enabled in the separate `glide-config` repo.
- **Release identity:** Keep upstream package version `0.2.15` and use `fork-v<upstream>-r<N>` for paired source/config checkpoints.
- **Runtime safety:** Automated work must not launch the live window manager; use tests, replay artifacts, or user-led runtime checkpoints.
- **Product direction:** Combine persistent tiling trees with Rectangle-style mouse placement instead of becoming only a tiler or only a snapper.

## 3. Known Issues & Backlog Notes

- Fixed halves/quadrants and tree split/group targets exist, but user-defined zones, per-orientation presets, target priority, and repeated-action cycles do not.
- Reserved empty tiles—the ability to leave a placeholder slot for the next window—are specified in the roadmap but not implemented.
- Tiled windows can be split or grouped by mouse, but there is no direct mouse swap/reinsert gesture for rearranging existing leaves.
- Window rules classify floating behavior and route into Worksets, but do not yet route into native Spaces, named containers, or saved Recipes.
- The combined Alt+digit switch-or-cycle interaction remains experimental; the stable setup uses separate native Space and Glide Workset shortcuts.
- There is no user-facing undo history for accidental tree restructuring.
- Focus and group navigation exist, but recent-focus navigation, marks, scratchpads, sticky windows, and a searchable window switcher do not.

## 4. Next Concrete Steps (Ordered)

1. [ ] Continue runtime-checking Chrome/Chromium transient surfaces and Workset switching over normal work sessions.
2. [ ] Specify configurable snap targets: activation region, destination frame, display orientation, modifier, priority, and repeated-action cycle.
3. [ ] Design the versioned read-only query schema and selectors that Workset Recipes will depend on.
4. [ ] Generalize live Worksets into portable single-display `save current` and `apply` Recipes.
5. [ ] Investigate pinned-window scopes and confirm which behaviors are possible without unsupported macOS APIs.
