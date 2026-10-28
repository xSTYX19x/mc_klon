use crate::app::App;
use crate::render::State;
use crate::world::time::TICKS_PER_DAY;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CommandDoc {
    name: &'static str,
    aliases: &'static [&'static str],
    usage: &'static str,
    summary: &'static str,
}

const COMMAND_DOCS: &[CommandDoc] = &[
    CommandDoc {
        name: "help",
        aliases: &[],
        usage: "/help",
        summary: "Shows the currently available chat commands.",
    },
    CommandDoc {
        name: "gamemode",
        aliases: &["gm"],
        usage: "/gamemode <creative|survival>",
        summary: "Switches between Creative and Survival mode.",
    },
    CommandDoc {
        name: "time",
        aliases: &[],
        usage: "/time set <day|noon|night|midnight|ticks>",
        summary: "Sets the world time to a named preset or explicit ticks.",
    },
    CommandDoc {
        name: "tp",
        aliases: &["teleport"],
        usage: "/tp <x> <y> <z> or /tp <x> <z>",
        summary: "Teleports the player; the 2-argument form keeps the current feet Y.",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GameModeCommand {
    Creative,
    Survival,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Command {
    Help,
    Gamemode { mode: GameModeCommand },
    TimeSet { ticks: u32 },
    Tp { x: f32, y: Option<f32>, z: f32 },
}

pub(crate) fn parse_command(line: &str) -> Result<Command, String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('/') {
        return Err("commands must start with '/'".to_string());
    }

    let mut parts = trimmed.split_whitespace();
    let raw_name = parts.next().unwrap_or_default();
    let name = raw_name.trim_start_matches('/').to_ascii_lowercase();
    if name.is_empty() {
        return Err("empty command".to_string());
    }

    match name.as_str() {
        "help" => {
            if parts.next().is_some() {
                return Err("usage: /help".to_string());
            }
            Ok(Command::Help)
        }
        "gamemode" | "gm" => {
            let Some(mode_raw) = parts.next() else {
                return Err("usage: /gamemode <creative|survival>".to_string());
            };
            if parts.next().is_some() {
                return Err("usage: /gamemode <creative|survival>".to_string());
            }
            let mode = parse_gamemode(mode_raw)?;
            Ok(Command::Gamemode { mode })
        }
        "time" => {
            let Some(sub) = parts.next() else {
                return Err("usage: /time set <day|noon|night|midnight|ticks>".to_string());
            };
            if !sub.eq_ignore_ascii_case("set") {
                return Err("usage: /time set <day|noon|night|midnight|ticks>".to_string());
            }
            let Some(value) = parts.next() else {
                return Err("usage: /time set <day|noon|night|midnight|ticks>".to_string());
            };
            if parts.next().is_some() {
                return Err("usage: /time set <day|noon|night|midnight|ticks>".to_string());
            }
            let ticks = parse_time_set_ticks(value)?;
            Ok(Command::TimeSet { ticks })
        }
        "tp" | "teleport" => {
            let args = parts.collect::<Vec<_>>();
            match args.as_slice() {
                [x, z] => Ok(Command::Tp {
                    x: parse_f32_arg("x", x)?,
                    y: None,
                    z: parse_f32_arg("z", z)?,
                }),
                [x, y, z] => Ok(Command::Tp {
                    x: parse_f32_arg("x", x)?,
                    y: Some(parse_f32_arg("y", y)?),
                    z: parse_f32_arg("z", z)?,
                }),
                _ => Err("usage: /tp <x> <y> <z> or /tp <x> <z>".to_string()),
            }
        }
        _ => Err(format!("unknown command '/{}'", name)),
    }
}

pub(crate) fn execute_command(
    app: &mut App,
    state: &mut State,
    cmd: Command,
) -> Result<String, String> {
    match cmd {
        Command::Help => Ok(help_text()),
        Command::Gamemode { mode } => {
            match mode {
                GameModeCommand::Creative => app.command_set_creative_mode(),
                GameModeCommand::Survival => app.command_set_survival_mode(state),
            }
            Ok(format!("Set game mode to {}.", gamemode_label(mode)))
        }
        Command::TimeSet { ticks } => {
            let day_base = (state.world_time.ticks_total / TICKS_PER_DAY as f64).floor()
                * TICKS_PER_DAY as f64;
            state.world_time.ticks_total = day_base + ticks as f64;
            state.world_time.recompute(0.0);
            Ok(format!("Set time to {}.", ticks))
        }
        Command::Tp { x, y, z } => {
            app.command_teleport_feet(x, y, z);
            Ok(format!(
                "Teleported to {:.2}, {:.2}, {:.2}.",
                app.player_feet_position().x,
                app.player_feet_position().y,
                app.player_feet_position().z
            ))
        }
    }
}

fn parse_f32_arg(name: &str, value: &str) -> Result<f32, String> {
    value
        .parse::<f32>()
        .map_err(|_| format!("invalid {} coordinate '{}'", name, value))
}

fn parse_gamemode(value: &str) -> Result<GameModeCommand, String> {
    match value.to_ascii_lowercase().as_str() {
        "creative" | "c" | "1" => Ok(GameModeCommand::Creative),
        "survival" | "s" | "0" => Ok(GameModeCommand::Survival),
        _ => Err(format!(
            "invalid gamemode '{}', expected creative/survival or 1/0",
            value
        )),
    }
}

fn parse_time_set_ticks(value: &str) -> Result<u32, String> {
    let lower = value.to_ascii_lowercase();
    match lower.as_str() {
        "day" => Ok(1000),
        "noon" => Ok(6000),
        "night" => Ok(13000),
        "midnight" => Ok(18000),
        _ => {
            let parsed = value
                .parse::<i32>()
                .map_err(|_| format!("invalid time '{}'", value))?;
            if !(0..=23_999).contains(&parsed) {
                return Err("time ticks must be in range 0..23999".to_string());
            }
            Ok(parsed as u32)
        }
    }
}

fn gamemode_label(mode: GameModeCommand) -> &'static str {
    match mode {
        GameModeCommand::Creative => "Creative",
        GameModeCommand::Survival => "Survival",
    }
}

fn help_text() -> String {
    let mut out = String::from("Commands:");
    for doc in COMMAND_DOCS {
        out.push_str("\n- ");
        out.push_str(doc.usage);
        if !doc.aliases.is_empty() {
            out.push_str(" (aliases: ");
            out.push_str(&doc.aliases.join(", "));
            out.push(')');
        }
        out.push_str(": ");
        out.push_str(doc.summary);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{COMMAND_DOCS, help_text};
    use std::fs;
    use std::path::PathBuf;

    fn commands_doc_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/reference/commands.md")
    }

    #[test]
    fn help_text_mentions_all_commands() {
        let help = help_text();
        for doc in COMMAND_DOCS {
            assert!(
                help.contains(doc.usage),
                "help output missing usage '{}'",
                doc.usage
            );
            assert!(
                help.contains(doc.summary),
                "help output missing summary '{}'",
                doc.summary
            );
        }
    }

    #[test]
    fn commands_doc_lists_every_registered_command() {
        let path = commands_doc_path();
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        for doc in COMMAND_DOCS {
            assert!(
                contents.contains(doc.usage),
                "commands doc missing usage '{}'",
                doc.usage
            );
            assert!(
                contents.contains(doc.summary),
                "commands doc missing summary '{}'",
                doc.summary
            );
        }
    }
}
