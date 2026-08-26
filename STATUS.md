# Status — Glide Fork

**Last updated:** 2026-08-26
**Current objective:** Evolve the stable private fork into a hybrid tiling and snapping window manager for keyboard and mouse power users.
**Overall state:** Fork release `fork-v0.2.15-r3` published · 273 library tests passing · Space 2 Workset prototype prioritized

---

## 1. Verified Completed Outputs

- `src/actor/reactor.rs` and `src/actor/layout.rs`: reliable remembered focus on multi-display Space changes, mouse drop handling, floating restoration, screen snapping, and group navigation.
- `src/model/layout_tree.rs` and `src/model/size.rs`: tree/group manipulation, exact proportions, balancing, automatic root orientation, and configurable gapless fullscreen/single-window layouts.
- `src/actor/drop_preview.rs` and `src/ui/group_bar.rs`: visual drop previews and indicators for stacked/tabbed groups.
- `glide.default.toml`: upstream-compatible defaults for fork commands and settings.
- `FORK.md` and `CHANGELOG.md`: fork behavior, release history, and versioning documented through `fork-v0.2.15-r3`.
- `TODO.md`: make-a-change roadmap prioritizing Worksets, precision placement, a scriptable API, Recipes, and pinned windows.
- `WORKSETS.md`: canonical Space, Workset, Layout, Window, Role, Slot, Recipe, pinning, and Stage Manager ontology.

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
- Window rules currently classify floating behavior; they do not route windows into Spaces, Worksets, named containers, or saved Recipes.
- There is no user-facing undo history for accidental tree restructuring.
- Focus and group navigation exist, but recent-focus navigation, marks, scratchpads, sticky windows, and a searchable window switcher do not.

## 4. Next Concrete Steps (Ordered)

1. [ ] Prototype Space 2 with Agents and Terminal Worksets, `Alt+2` cycling, and direct `Alt+T` selection.
2. [ ] Specify R4 configurable snap targets: activation region, destination frame, display orientation, modifier, priority, and repeated-action cycle.
3. [ ] Design the versioned read-only query schema and selectors that Workset Recipes will depend on.
4. [ ] Generalize the Workset prototype into portable single-display `save current` and `apply` Recipes.
5. [ ] Investigate pinned-window scopes and confirm which behaviors are possible without unsupported macOS APIs.
