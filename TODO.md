---
schema: make-a-change/todo/v1
---

# Todo

Actionable development roadmap for the Glide fork.
Format adheres to [make-a-change](https://github.com/alexsmedile/make-a-change).
Canonical feature terminology is defined in [WORKSETS.md](WORKSETS.md).

The product direction is a hybrid of persistent tiling, precise snapping, and
direct mouse editing. Each parent item needs a narrow prototype and a user
checkpoint before it earns a `feat/*` branch.

## Now

- [ ] [prototype] Test Space 2 with Agents and Terminal Worksets <!-- ref: #4, from: #1 #3 -->
  - [ ] Model an ordered Workset collection containing an Agents split tree and a Terminal tree
  - [ ] Show Codex and Gemini together in the Agents Workset and the general Ghostty window in the Terminal Workset
  - [ ] Preserve focus and proportions independently inside each Workset
  - [ ] Use ephemeral window ids for the first session before adding durable title/role/mark matching
  - [ ] Make `Alt+2` switch to Space 2 from elsewhere and cycle Worksets when Space 2 is already active
  - [ ] Make `Alt+T` switch to Space 2 and select the Terminal Workset directly
  - [ ] Keep the old Space 3 and macOS/`skhd` bindings recoverable until the interaction is verified
- [ ] [worksets] Establish the Workset domain model <!-- ref: #5, from: #4 -->
  - [ ] Introduce stable `WorksetId` identity above Layout selection
  - [ ] Store an ordered Workset collection and one active Workset per Space
  - [ ] Let each Workset own membership, tiled Layout, floating frames, focus, selection, placeholders, and runtime policy
  - [ ] Keep Worksets live and usable without persistent Recipes
  - [ ] Switch Worksets as one transaction that suppresses the old set before surfacing and focusing the new set
  - [ ] Preserve inactive Workset state without raising or rearranging its windows
  - [ ] Keep screen-size Layout variants subordinate to Workset identity
  - [ ] Define deterministic model tests for activation, isolation, focus restoration, missing windows, and rollback
- [ ] [placement] Implement R4 declarative precision-placement targets
  - [ ] Define activation regions independently from destination frames
  - [ ] Select different target sets for portrait and landscape displays
  - [ ] Ship halves, quadrants, thirds, two-thirds, maximize, center, and arbitrary fractional rectangles as configurable presets
  - [ ] Cycle repeated executions, such as half → two-thirds → one-third
  - [ ] Resolve overlapping activation regions with explicit target priority
  - [ ] Enable or suppress targets based on configured modifiers
  - [ ] Let each target produce either a floating frame or a tiled-tree change
  - [ ] Preview the exact result through the existing drop overlay
  - [ ] Validate impossible fractions, ambiguous overlaps, and unknown actions at config load
  - [ ] Use the saved Rectangle configuration in `~/code/utils/rectangle` as the migration reference
- [ ] [api] Design a versioned scriptable query and event API
  - [ ] Query displays, active Spaces, layouts, containers, windows, focus, floating state, groups, proportions, marks, and pinned state as stable JSON
  - [ ] Select windows by id, bundle id, app name, title, role, Space, mark, or current focus
  - [ ] Execute public Glide commands against an explicit selector
  - [ ] Subscribe to window, focus, Space, display, layout, and config events
  - [ ] Emit events only after Reactor state is coherent and the corresponding operation is committed
  - [ ] Version the schema and return clear errors for stale ids or incompatible clients
  - [ ] Bound event queues and command timeouts
  - [ ] Keep all transport and I/O in the actor layer
  - [ ] Make the CLI deterministic enough for `jq`, shell scripts, Raycast, SketchyBar, and replay tests
- [ ] [recipes] Implement persistent Recipes for Worksets <!-- ref: #1 -->
  - [ ] Support explicit `apply_once` and persistent `maintain` recipe modes
  - [ ] Save the current tree, orientations, groups, proportions, gaps, placeholders, floating frames, and selected role under a name
  - [ ] Match recipe windows by bundle id, title, and role instead of ephemeral window ids
  - [ ] Restore a recipe on one display with explicit policies for missing and extra windows
  - [ ] Adapt fractional geometry when display dimensions change
  - [ ] Optionally launch missing apps and wait for their windows through the event API
  - [ ] Target displays by stable identity, relative order, or orientation
  - [ ] Activate recipes by shortcut, CLI/API call, login, or display connection change
  - [ ] Keep the recipe format editable, portable, validated, and separate from internal crash-restore snapshots
- [ ] [spaces] Let Glide own Space shortcuts and per-Space Workset cycling <!-- ref: #3, from: #1 -->
  - [ ] Add a Glide command that activates a Space by configured logical name or index
  - [ ] Switch and restore the last-used Workset when the requested Space is not active
  - [ ] Cycle Worksets forward when the requested Space is already active and reverse with Shift
  - [ ] Show a short-lived Space/Workset indicator and remember the last Workset per Space
  - [ ] Perform native Space switching in the system layer without synthesizing macOS shortcuts
  - [ ] Keep switch-versus-cycle policy in `SpaceManager` and activate Worksets only after the destination is confirmed active
  - [ ] Detect conflicting macOS Mission Control bindings and document their manual removal
  - [ ] Preserve focus memory and coalesce repeated requests during in-flight Space transitions
- [ ] [stage-manager] Integrate Stage Manager as an optional Workset adapter <!-- ref: #2, from: #1 -->
  - [ ] Keep every Workset and Recipe usable when Stage Manager is disabled
  - [ ] Treat the active Stage Manager group as a visibility boundary and arrange only its visible windows
  - [ ] Restore a recognized group's remembered layout and focus after its visible window set settles
  - [ ] Support an optional Stage Manager shelf/reserved-area policy
  - [ ] Keep native group creation or switching experimental if it requires private APIs or Accessibility UI scripting
- [ ] [pinning] Prototype pinned windows
  - [ ] Define all-Spaces, follow-focus, display-pinned, and layout-pinned scopes separately
  - [ ] Determine whether each scope permits tiling or requires floating
  - [ ] Define interactions with fullscreen, groups, Mission Control, and multiple displays
  - [ ] Restore the original Space, tree position, and floating frame when unpinned
  - [ ] Verify which native all-Spaces behaviors require unsupported private APIs
  - [ ] Keep a follow-focus fallback implementable through existing actors
  - [ ] Distinguish pinned windows from always-on-top, scratchpads, and Rectangle-style pin mode

## Next

- [ ] [layout] Implement reserved empty tiles
  - [ ] Model placeholders as real tree nodes without fake `WindowId` values
  - [ ] Support layouts such as `[empty: 1/3][window: 2/3]`
  - [ ] Let the next window, next matching app, or a manual drop claim a placeholder
  - [ ] Preserve placeholders across rebalancing, restart, display-size variants, and recipe export
  - [ ] Require an explicit action or configured timeout before removing a placeholder
- [ ] [layout] Add mouse swap and reinsert operations for tiled windows
  - [ ] Swap two leaves without changing their containers
  - [ ] Reinsert before or after a target and shift its siblings
  - [ ] Move a selected container when selection has ascended
  - [ ] Give swap, insertion, split, and group operations visually distinct previews
- [ ] [history] Add bounded per-Space undo and redo for layout mutations
  - [ ] Record split, group, ungroup, move, swap, float, snap, resize, balance, placeholder, and recipe operations
  - [ ] Exclude focus-only changes from history
  - [ ] Undo a compound mouse drop or recipe application as one transaction
- [ ] [ui] Communicate window mode with short-lived color feedback
  - [ ] Keep blue for floating, purple for tiled splits, and green for stacked groups
  - [ ] Show the mode briefly when a window is selected or changes mode
  - [ ] Remain distinguishable from macOS focus, urgent indicators, and the separate Borders tool
- [ ] [rules] Extend window rules into workspace routing
  - [ ] Match bundle id, title, role, and subrole
  - [ ] Route new windows to a Space, placeholder, marked container, group, recipe role, or pinned policy
  - [ ] Add an explain command that reports which rule matched and why
- [ ] [scratchpad] Implement edge stashes and named scratchpads
  - [ ] Stash a window at an edge while remembering its tree slot and floating frame
  - [ ] Prototype one stash per edge before supporting edge stacks
  - [ ] Define whether each stash is Space-local, display-local, or follows focus
  - [ ] Toggle named app/window roles such as `scratchpad terminal` from any Space
- [ ] [navigation] Add recent-focus history
  - [ ] Navigate recent windows within the current Space, display, or globally
  - [ ] Preserve existing geometric and group-local navigation
- [ ] [navigation] Add mark-and-jump workflows
  - [ ] Assign a stable user mark to a window or placeholder
  - [ ] Jump to the mark or move another window into its slot
  - [ ] Persist recipe roles rather than ephemeral window ids where appropriate
- [ ] [navigation] Add an explicit insertion point for the next window
  - [ ] Choose the target node and insertion direction before opening a window
  - [ ] Reuse reserved placeholder nodes instead of creating a second mechanism
  - [ ] Show a visible preselection indicator

## Later

- [ ] [ui] Add a searchable window switcher
  - [ ] Filter by application, title, Space, group, mark, or pinned state
  - [ ] Switch Space, reveal the correct group member, and focus it as one operation
- [ ] [display] Move or swap whole layouts between displays
  - [ ] Prefer moving Glide layout state over manipulating native macOS Spaces
  - [ ] Preserve proportions and adapt geometry to the destination display
- [ ] [layout] Add per-Space gaps and padding profiles
  - [ ] Set, nudge, or toggle inner gaps and each outer edge independently
  - [ ] Save and restore the profile through Workset Recipes
- [ ] [diagnostics] Add a permission and runtime health check
  - [ ] Report bundle identity and signing, Accessibility permission, running binary and fork version, config compatibility, IPC health, and launch-agent state
  - [ ] Distinguish a missing permission from a stale or re-signed application identity
  - [ ] Provide safe recovery instructions without mutating system state
- [ ] [config] Add explicit migrations for complex targets and recipes
  - [ ] Version target and recipe schemas
  - [ ] Report deprecated fields and generate a safe migration preview

## Done (Unreleased)
