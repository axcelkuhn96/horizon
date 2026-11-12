use std::io::Write;
use std::path::{Path, PathBuf};

use horizon_core::HorizonHome;

struct EmbeddedFile {
    relative_path: &'static str,
    content: &'static str,
}

const CLAUDE_PLUGIN_FILES: &[EmbeddedFile] = &[
    EmbeddedFile {
        relative_path: ".claude-plugin/plugin.json",
        content: include_str!(concat!(
            env!("OUT_DIR"),
            "/assets/plugins/claude-code/.claude-plugin/plugin.json"
        )),
    },
    EmbeddedFile {
        relative_path: "skills/horizon-notify/SKILL.md",
        content: include_str!(concat!(
            env!("OUT_DIR"),
            "/assets/plugins/claude-code/skills/horizon-notify/SKILL.md"
        )),
    },
    EmbeddedFile {
        relative_path: "skills/horizon-link/SKILL.md",
        content: include_str!(concat!(
            env!("OUT_DIR"),
            "/assets/plugins/claude-code/skills/horizon-link/SKILL.md"
        )),
    },
];

const TEXT_SKILL_FILES: &[EmbeddedFile] = &[EmbeddedFile {
    relative_path: "SKILL.md",
    content: include_str!(concat!(
        env!("OUT_DIR"),
        "/assets/plugins/codex/skills/horizon-notify/SKILL.md"
    )),
}];

pub(crate) fn install_agent_plugins(horizon_home: &HorizonHome) {
    let user_home = std::env::var_os("HOME").map(PathBuf::from);

    match install_agent_plugins_impl(horizon_home, user_home.as_deref()) {
        Ok(updated_files) if updated_files > 0 => {
            tracing::info!(updated_files, "synced embedded Horizon agent plugins");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!("failed to sync embedded Horizon agent plugins: {error}"),
    }
}

fn install_agent_plugins_impl(horizon_home: &HorizonHome, user_home: Option<&Path>) -> std::io::Result<usize> {
    let mut updated_files = 0usize;

    updated_files += sync_plugin_files(&horizon_home.claude_plugin_dir(), CLAUDE_PLUGIN_FILES)?;
    // Without a resolvable executable the hooks cannot be written; the status then comes from
    // Claude's session file alone.
    if let Ok(exe) = std::env::current_exe() {
        updated_files += install_claude_hooks(horizon_home, &exe)?;
    }
    updated_files += sync_plugin_files(&horizon_home.codex_skill_dir(), TEXT_SKILL_FILES)?;

    if let Some(home) = user_home {
        let codex_home_skill_dir = home.join(".codex").join("skills").join("horizon-notify");
        updated_files += sync_plugin_files(&codex_home_skill_dir, TEXT_SKILL_FILES)?;
        let codex_export_dir = home.join(".agents").join("skills").join("horizon-notify");
        updated_files += sync_plugin_files(&codex_export_dir, TEXT_SKILL_FILES)?;
        let kilo_export_dir = home.join(".kilocode").join("skills").join("horizon-notify");
        updated_files += sync_plugin_files(&kilo_export_dir, TEXT_SKILL_FILES)?;
    }

    Ok(updated_files)
}

/// Relative path of the Claude plugin's hooks file.
const CLAUDE_HOOKS_FILE: &str = "hooks/hooks.json";

/// `exe` as one shell word: single quotes, with embedded single quotes closed, escaped and reopened.
fn shell_quote(exe: &Path) -> String {
    format!("'{}'", exe.to_string_lossy().replace('\'', "'\\''"))
}

/// Contents of the Claude plugin's `hooks/hooks.json`: every hook runs `<exe> hook <event>`.
///
/// `PostToolUse` reports `working` again so a permission answer leaves the `waiting` state;
/// `Notification` is limited to the prompts that block on the user.
pub(crate) fn render_hooks_json(exe: &Path) -> String {
    let quoted = shell_quote(exe);
    let hook = |event: &str, matcher: Option<&str>| {
        let mut entry = serde_json::json!({
            "hooks": [{
                "type": "command",
                "command": format!("{quoted} hook {event}"),
                "timeout": 5,
            }],
        });
        if let (Some(matcher), Some(object)) = (matcher, entry.as_object_mut()) {
            object.insert("matcher".to_owned(), serde_json::Value::String(matcher.to_owned()));
        }
        serde_json::Value::Array(vec![entry])
    };
    let document = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": hook("working", None),
            "PreToolUse": hook("working", Some("*")),
            "PostToolUse": hook("working", Some("*")),
            "Notification": hook("waiting", Some("permission_prompt|elicitation_dialog")),
            "Stop": hook("done", None),
            "StopFailure": hook("error", None),
            "SessionStart": hook("session-start", Some("startup|resume|clear")),
            "SessionEnd": hook("session-end", None),
        },
    });
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_owned());
    text.push('\n');
    text
}

/// Writes the Claude plugin's hooks file for `exe`; rewritten when the executable moved.
fn install_claude_hooks(horizon_home: &HorizonHome, exe: &Path) -> std::io::Result<usize> {
    let path = horizon_home.claude_plugin_dir().join(CLAUDE_HOOKS_FILE);
    Ok(usize::from(sync_file_if_changed(&path, &render_hooks_json(exe))?))
}

fn sync_plugin_files(base: &Path, files: &[EmbeddedFile]) -> std::io::Result<usize> {
    let mut updated_files = 0usize;

    for embedded_file in files {
        let path = base.join(embedded_file.relative_path);
        if sync_file_if_changed(&path, embedded_file.content)? {
            updated_files += 1;
        }
    }

    Ok(updated_files)
}

fn sync_file_if_changed(path: &Path, content: &str) -> std::io::Result<bool> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let mut temp_file = tempfile::NamedTempFile::new_in(parent)?;
    temp_file.write_all(content.as_bytes())?;
    temp_file.flush()?;
    temp_file.persist(path).map_err(|error| error.error)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use horizon_core::HorizonHome;

    use std::path::Path;

    use super::{
        CLAUDE_HOOKS_FILE, EmbeddedFile, TEXT_SKILL_FILES, install_agent_plugins_impl, install_claude_hooks,
        render_hooks_json, sync_file_if_changed, sync_plugin_files,
    };

    #[test]
    fn sync_file_if_changed_writes_missing_file() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("skill").join("SKILL.md");

        let updated = sync_file_if_changed(&path, "version-1").expect("write file");

        assert!(updated);
        assert_eq!(std::fs::read_to_string(path).expect("read file"), "version-1");
    }

    #[test]
    fn sync_file_if_changed_skips_identical_content() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("SKILL.md");
        std::fs::write(&path, "same").expect("seed file");

        let updated = sync_file_if_changed(&path, "same").expect("sync file");

        assert!(!updated);
    }

    #[test]
    fn sync_plugin_files_reports_only_changed_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let files = [
            EmbeddedFile {
                relative_path: "a.txt",
                content: "alpha",
            },
            EmbeddedFile {
                relative_path: "nested/b.txt",
                content: "beta",
            },
        ];

        let first = sync_plugin_files(temp.path(), &files).expect("first sync");
        let second = sync_plugin_files(temp.path(), &files).expect("second sync");

        assert_eq!(first, 2);
        assert_eq!(second, 0);
    }

    #[test]
    fn plugin_install_writes_horizon_link_skill() {
        let temp = tempfile::tempdir().expect("temp dir");
        let horizon_home = HorizonHome::from_root(temp.path().join(".horizon"));

        install_agent_plugins_impl(&horizon_home, None).expect("install plugins");

        let skill = std::fs::read_to_string(horizon_home.claude_plugin_dir().join("skills/horizon-link/SKILL.md"))
            .expect("horizon-link skill should be installed");
        assert!(skill.starts_with("---\nname: horizon-link\n"));
        assert!(skill.contains("description:"));
        assert!(skill.contains("HORIZON_PANEL_NAME"));
    }

    #[test]
    fn install_agent_plugins_syncs_codex_skill_into_current_codex_home() {
        let temp = tempfile::tempdir().expect("temp dir");
        let horizon_home = HorizonHome::from_root(temp.path().join(".horizon"));
        let user_home = temp.path().join("user-home");

        let updated = install_agent_plugins_impl(&horizon_home, Some(&user_home)).expect("install plugins");

        assert!(updated > 0);
        assert_eq!(
            std::fs::read_to_string(user_home.join(".codex/skills/horizon-notify/SKILL.md"))
                .expect("codex skill should be exported"),
            TEXT_SKILL_FILES[0].content,
        );
        assert_eq!(
            std::fs::read_to_string(user_home.join(".agents/skills/horizon-notify/SKILL.md"))
                .expect("legacy codex skill export should still exist"),
            TEXT_SKILL_FILES[0].content,
        );
        assert_eq!(
            std::fs::read_to_string(user_home.join(".kilocode/skills/horizon-notify/SKILL.md"))
                .expect("kilo skill should be exported"),
            TEXT_SKILL_FILES[0].content,
        );
        assert_eq!(
            std::fs::read_to_string(horizon_home.codex_skill_dir().join("SKILL.md"))
                .expect("horizon codex integration should be synced"),
            TEXT_SKILL_FILES[0].content,
        );
    }

    fn hook_commands(document: &serde_json::Value, event: &str) -> Vec<String> {
        document["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|entry| entry["hooks"].as_array().into_iter().flatten())
            .filter_map(|hook| hook["command"].as_str().map(str::to_owned))
            .collect()
    }

    #[test]
    fn render_hooks_json_is_valid_json_and_quotes_exe_with_spaces_and_quotes() {
        let exe = Path::new("/opt/My Apps/it's horizon/horizon");

        let text = render_hooks_json(exe);
        let document: serde_json::Value = serde_json::from_str(&text).expect("valid json");

        let commands = hook_commands(&document, "Stop");
        assert_eq!(
            commands,
            vec!["'/opt/My Apps/it'\\''s horizon/horizon' hook done".to_owned()]
        );
        assert_eq!(document["hooks"]["Stop"][0]["hooks"][0]["timeout"], 5);
        assert_eq!(document["hooks"]["Stop"][0]["hooks"][0]["type"], "command");
    }

    #[test]
    fn render_hooks_json_maps_events() {
        let document: serde_json::Value =
            serde_json::from_str(&render_hooks_json(Path::new("/bin/horizon"))).expect("valid json");

        for (event, state) in [
            ("UserPromptSubmit", "working"),
            ("PreToolUse", "working"),
            ("PostToolUse", "working"),
            ("Notification", "waiting"),
            ("Stop", "done"),
            ("StopFailure", "error"),
            ("SessionStart", "session-start"),
            ("SessionEnd", "session-end"),
        ] {
            assert_eq!(
                hook_commands(&document, event),
                vec![format!("'/bin/horizon' hook {state}")],
                "event {event}"
            );
        }
        assert_eq!(
            document["hooks"]["Notification"][0]["matcher"],
            "permission_prompt|elicitation_dialog"
        );
        assert!(document["hooks"]["Stop"][0].get("matcher").is_none());
        // `compact` also fires SessionStart mid-turn; it must not read as a finished session.
        assert_eq!(document["hooks"]["SessionStart"][0]["matcher"], "startup|resume|clear");
    }

    #[test]
    fn plugin_install_writes_rendered_hooks_json() {
        let temp = tempfile::tempdir().expect("temp dir");
        let horizon_home = HorizonHome::from_root(temp.path().join(".horizon"));
        let exe = Path::new("/usr/local/bin/horizon");

        assert_eq!(install_claude_hooks(&horizon_home, exe).expect("install"), 1);

        let written = std::fs::read_to_string(horizon_home.claude_plugin_dir().join(CLAUDE_HOOKS_FILE))
            .expect("hooks file should be installed");
        assert_eq!(written, render_hooks_json(exe));
        assert_eq!(
            install_claude_hooks(&horizon_home, exe).expect("second install"),
            0,
            "unchanged hooks are not rewritten"
        );
    }

    #[test]
    fn plugin_install_hooks_json_updates_when_exe_path_changes() {
        let temp = tempfile::tempdir().expect("temp dir");
        let horizon_home = HorizonHome::from_root(temp.path().join(".horizon"));
        install_claude_hooks(&horizon_home, Path::new("/old/horizon")).expect("first install");

        let updated = install_claude_hooks(&horizon_home, Path::new("/new/horizon")).expect("second install");

        assert_eq!(updated, 1);
        let written =
            std::fs::read_to_string(horizon_home.claude_plugin_dir().join(CLAUDE_HOOKS_FILE)).expect("hooks file");
        assert!(written.contains("'/new/horizon' hook working"));
        assert!(!written.contains("/old/horizon"));
    }

    #[test]
    fn full_install_writes_the_hooks_file_for_the_running_executable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let horizon_home = HorizonHome::from_root(temp.path().join(".horizon"));

        install_agent_plugins_impl(&horizon_home, None).expect("install plugins");

        assert!(horizon_home.claude_plugin_dir().join(CLAUDE_HOOKS_FILE).is_file());
    }
}
