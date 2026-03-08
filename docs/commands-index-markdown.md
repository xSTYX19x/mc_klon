# Commands Index

Source of truth:
- `src/commands/mod.rs`

This file is expected to stay in sync with the registered chat commands.
`cargo test commands_doc_lists_every_registered_command -- --nocapture` fails if a command is added or changed without updating this document.

## Available Commands

| Command | Aliases | Usage | Effect |
|---|---|---|---|
| `help` | `-` | `/help` | Shows the currently available chat commands. |
| `gamemode` | `gm` | `/gamemode <creative|survival>` | Switches between Creative and Survival mode. |
| `time` | `-` | `/time set <day|noon|night|midnight|ticks>` | Sets the world time to a named preset or explicit ticks. |
| `tp` | `teleport` | `/tp <x> <y> <z> or /tp <x> <z>` | Teleports the player; the 2-argument form keeps the current feet Y. |

## Notes

- Commands must start with `/`.
- Unknown commands and invalid arguments currently return a usage/error message in chat.
- Coordinate parsing for `/tp` currently uses decimal `f32` values.
