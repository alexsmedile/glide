# Workset ontology

This document defines the vocabulary and semantic model for multiple switchable
window environments inside one macOS Space.

The user-facing metaphor is:

| Glide concept | Metaphor |
|---|---|
| macOS Space | Building |
| Workset | Floor inside the building |
| Window | Room on the floor |
| Layout | Arrangement of the floor's rooms |
| Recipe | Blueprint used to create or restore a floor |

The metaphor explains the feature, but product UI, configuration, commands, and
code should use the canonical terms below.

## Canonical hierarchy

```text
Display
└── active macOS Space
    ├── active Workset
    │   ├── tiled Layout
    │   │   ├── Split containers
    │   │   ├── Group containers
    │   │   └── Window slots
    │   ├── floating windows
    │   └── focus and selection state
    └── inactive Worksets
```

A display exposes one active macOS Space. A Space has an ordered collection of
Worksets and exposes one active Workset at a time.

## Definitions

### Space

A native macOS Space, identified by `SpaceId` and associated with one display
when “Displays have separate Spaces” is enabled.

Glide does not redefine or emulate Spaces. Switching Spaces changes the native
macOS desktop. Each Space remembers its last active Workset.

### Workset

A live, named window environment inside one Space.

A Workset owns:

- membership: the windows and expected roles belonging to it;
- a tiled Layout tree;
- floating windows and their frames;
- selected and focused state;
- placeholders for expected windows;
- runtime policy such as `apply_once` or `maintain`.

Exactly one Workset is active in a Space. Inactive Worksets retain their state
but must not be raised or allowed to obscure the active Workset.

A Workset can exist without a Recipe. For example, the first prototype may
capture the currently open windows using ephemeral window ids.

### Layout

The geometric arrangement of a Workset's tiled window slots.

A Layout is a rooted tree. It does not represent the Workset's identity,
activation rules, floating layer, or external automation.

### Container

An internal Layout node that arranges child nodes. Containers can contain
windows, placeholders, or other containers.

There are two semantic families:

- **Split container:** horizontal or vertical; all children are visible and
  divide the available rectangle.
- **Group container:** tabbed or stacked; children share a rectangle and only
  the selected child subtree is surfaced.

`tree` is the structure containing containers; it is not a container kind
alongside `group`.

### Window

One concrete macOS application window. A window is a Layout leaf when tiled.

Application and window are not synonyms: one application such as Ghostty can
own several distinct windows in one or more Worksets.

### Role

A stable semantic identity used to match a concrete window to a Recipe slot.
A role may use bundle id, title, accessibility role/subrole, user mark, or a
combination of those fields.

Examples: `codex`, `gemini`, `terminal-main`, and `browser-docs`.

### Slot

One expected position in a Layout. A slot has geometry, a role, and optionally
a bound Window.

An unoccupied slot is a Placeholder. Placeholder nodes must not use fabricated
`WindowId` values.

### Recipe

A persistent, editable blueprint that creates, restores, or reconciles a
Workset.

A Recipe may define:

- window roles and matching rules;
- Layout structure, groups, orientation, and proportions;
- placeholders and missing/extra-window policies;
- floating frames, gaps, and display preferences;
- optional app, file, URL, or command launches;
- activation triggers.

`apply_once` applies the blueprint and then stops governing manual changes.
`maintain` keeps reconciling the live Workset as matching windows appear or
disappear.

A Recipe is durable configuration. A Workset is its live runtime instance.

### Pinned window

A Window projected beyond one Workset's normal membership.

Pinning needs an explicit scope: all Spaces, follow focus, one display, or the
corresponding slot across multiple Worksets. A pinned window is not implicitly
floating, always-on-top, grouped, or a scratchpad.

### Stage Manager adapter

An optional adapter that maps the currently visible Stage Manager group to a
Workset activation boundary.

Stage Manager is not required by Spaces, Worksets, Layouts, or Recipes. The
adapter must not change the core ontology.

## Example geometry

```text
Space 2
├── Workset: Agents
│   └── Layout
│       └── Split(Horizontal)
│           ├── Window(role=codex)
│           └── Window(role=gemini)
└── Workset: Terminal
    └── Layout
        └── Split(Horizontal)
            ├── Group(Tabbed)
            │   ├── Window(role=ghostty-1)
            │   └── Window(role=ghostty-2)
            └── Split(Vertical)
                ├── Window(role=ghostty-3)
                └── Window(role=ghostty-4)
```

In the metaphor, Space 2 is the building, Agents and Terminal are floors, each
Window is a room, and each Layout describes how those rooms occupy the floor.

## Activation semantics

- Activating a different Space restores that Space's last active Workset.
- Requesting the already-active Space may cycle its ordered Worksets.
- Activating a Workset directly may first activate its containing Space.
- Cycling Worksets never changes Space.
- Each Workset remembers its own selected Window, focus, proportions, groups,
  floating frames, and placeholders.
- A Workset switch is one transaction: inactive windows are suppressed before
  active windows are surfaced and focused.

For the first prototype:

- `Alt+2` activates Space 2 or cycles its Worksets when already active.
- `Alt+T` activates the Terminal Workset in Space 2 directly.
- `Alt+A` may activate the Agents Workset directly.

## Naming rules

Use these terms consistently:

- Use **Space** only for native macOS Spaces.
- Use **Workset** for a live switchable window environment within a Space.
- Use **Layout** only for the geometric tree inside a Workset.
- Use **Recipe** only for the persistent blueprint.
- Use **Group** only for Glide's tabbed/stacked Layout container.
- Use **Window** for a concrete macOS window and **Application** for its owner.

Avoid `workspace`, `scene`, `layer`, and `session` in public APIs for this
feature. They overlap with existing window-manager, Stage Manager, rendering,
and terminal vocabulary.

## Architectural mapping

- `model`: Workset state, Layout trees, Recipe-to-Workset reconciliation, and
  deterministic activation transitions.
- `actor::layout::LayoutManager`: Workset policy, classification, role
  matching, and desired frames/focus.
- `actor::space_manager::SpaceManager`: Space activation, active Workset
  selection, switch-versus-cycle decisions, and in-flight request coalescing.
- `sys`: native Space switching and window-system operations only.
- `ui`: Workset indicator, Recipe progress, and errors.
- `config`: Recipe definitions, Workset ordering, role selectors, and
  shortcuts.

The existing `SpaceLayoutMapping` maps a Space and screen size to Layout
variants. Worksets introduce an additional identity above Layout selection; a
Workset must not be represented merely as another screen-size Layout variant.
