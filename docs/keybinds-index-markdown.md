# Keybinds Index

Source of truth:
- `src/app/mod.rs`
- `src/input.rs`
- `src/settings.rs`

## Global/UI

| Input | Action | Context |
|---|---|---|
| `Escape` | Toggle pause menu | In-game |
| `Escape` | Close chat | Chat open |
| `Escape` | Close inventory | Inventory open |
| `Escape` | Back / close options | Options menu |
| `E` | Toggle inventory | In-game (not paused/generating) |
| `F3` | Toggle debug overlay | In-game, paused, inventory |

## Gameplay

| Input | Action | Context |
|---|---|---|
| `W` `A` `S` `D` | Move | In-game |
| `Space` | Jump / move up (flight/swim) | In-game |
| `Shift` (`Left`/`Right`) | Sneak / move down | In-game |
| `Ctrl` (`Left`/`Right`) | Hold sprint | In-game |
| Double-tap `W` | Toggle sprint latch | In-game |
| `Mouse Move` | Look around | While mouse captured |
| `LMB` | Break block | In-game, mouse captured |
| `RMB` | Place block | In-game, mouse captured |
| `MMB` | Pick block into current hotbar slot | In-game, mouse captured |
| `Mouse Wheel` | Cycle hotbar slot | In-game |
| `Enter` / `NumpadEnter` | Save world | In-game |
| `N` | Generate new level | Not generating |
| `T` | Open chat | In-game |

## Ticking-Area Shortcut

| Input | Action | Context |
|---|---|---|
| `Ctrl + T` | Create ticking area radius 4 at player chunk | In-game |
| `Ctrl + Shift + T` | Remove nearest ticking area | In-game |

## Render / Debug Toggles

| Input | Action | Context |
|---|---|---|
| `F4` | Toggle debug depth test | In-game |
| `F6` | Toggle far LOD | In-game |
| `F7` | Toggle AO disable + remesh | In-game |
| `F8` | Cycle lighting debug view | In-game |
| `F9` | Print fluid debug line under crosshair | In-game |
| `F10` | Toggle rolling perf summary logger (1s) | In-game |
| `F` | Cycle render distance | In-game |
| `G` | Cycle simulation distance | In-game |

## First-Person Hand Tweak Mode

Enable/disable with `K`.

| Input | Action |
|---|---|
| `Mouse Wheel` | Hand scale |
| `ArrowLeft` / `ArrowRight` | Side offset - / + |
| `ArrowUp` / `ArrowDown` | Down offset - / + |
| `PageUp` / `PageDown` | Forward offset + / - |
| `[` / `]` | Roll - / + |
| `X` / `C` | Model rot X - / + |
| `Y` / `H` | Model rot Y - / + |
| `Z` / `V` | Model rot Z - / + |

## Pause Menu

| Input | Action |
|---|---|
| `ArrowUp` / `W` | Move selection up |
| `ArrowDown` / `S` | Move selection down |
| `Enter` / `NumpadEnter` / `Space` | Activate selected item |
| `LMB` | Activate hovered item |

## Main Menu / Options

| Input | Action |
|---|---|
| `ArrowUp` / `W` | Move selection up |
| `ArrowDown` / `S` | Move selection down |
| `ArrowLeft` / `A` | Change option value on supported rows |
| `ArrowRight` / `D` | Change option value on supported rows |
| `Enter` / `NumpadEnter` / `Space` | Activate selected item |
| `Escape` | Back / close current screen |
| `LMB` | Activate hovered item |
| any key while rebinding | Set selected control binding |

## Chat

| Input | Action |
|---|---|
| `Enter` / `NumpadEnter` | Submit message |
| `Backspace` | Delete previous character |
| `Delete` | Delete next character |
| `ArrowLeft` / `ArrowRight` | Move cursor |
| `ArrowUp` / `ArrowDown` | Navigate sent-history |
| `Home` / `End` | Jump to line start/end |

## Inventory

| Input | Action |
|---|---|
| `Mouse Wheel` | Scroll pages |
| `LMB` on palette | Select + start drag block |
| `LMB` release on hotbar slot | Drop dragged block into slot |
| `LMB` on hotbar slot | Select slot |
| `RMB` on hotbar slot | Clear slot |

## Notes

- Number keys `1..9` are currently **not** bound to hotbar selection (a helper exists in `src/input.rs`, but it is unused).
- Core movement/UI actions can be rebound through the Options -> Controls menu and are persisted in `save/settings.json`.
- Backface culling is configured through Options -> Video / Graphics and is persisted in `save/settings.json`.
- Rebinding conflicts are currently handled by swapping the existing assignment onto the displaced action.
- Most gameplay/debug binds are disabled while world generation is running.
