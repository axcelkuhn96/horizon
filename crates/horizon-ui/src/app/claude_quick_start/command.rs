//! The command line the quick-start types into the shell, and how it is quoted for that shell.

use std::path::Path;

use horizon_core::PresetConfig;
use horizon_core::agent_detect::default_claude_config_dir;

use super::QuickStartMode;

const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";
const DEFAULT_BINARY: &str = "claude";

/// Quoting and environment syntax of the shell that receives the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShellSyntax {
    /// bash, zsh, fish, nushell and friends: the environment goes through `env 'NAME=value'`,
    /// which every one of them runs as a plain program.
    Posix,
    /// The Windows default shell.
    PowerShell,
}

impl ShellSyntax {
    /// The syntax of the shell Horizon starts on this platform.
    pub(super) const NATIVE: Self = if cfg!(windows) { Self::PowerShell } else { Self::Posix };

    fn bare_char(self, c: char) -> bool {
        c.is_ascii_alphanumeric()
            || match self {
                Self::Posix => matches!(c, '-' | '_' | '.' | '/' | '=' | ':' | ',' | '@' | '%' | '+'),
                Self::PowerShell => matches!(c, '-' | '_' | '.' | '/' | '\\' | ':' | '+'),
            }
    }

    /// Characters that would change meaning (or end the line) inside this shell's single quotes.
    /// bash/zsh read `\` literally there but fish and nushell do not, and PowerShell treats its
    /// quote glyphs, backtick and `$` specially, so none of them is ever typed into the shell.
    fn unsafe_char(self, c: char) -> bool {
        c.is_control()
            || match self {
                Self::Posix => matches!(c, '\'' | '\\'),
                Self::PowerShell => matches!(c, '\'' | '`' | '$' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}'),
            }
    }

    /// Keystrokes that empty a half-typed prompt line before the command is typed: Ctrl-E (end
    /// of line) then Ctrl-U (kill to the start), so leftover text cannot prefix the command.
    /// POSIX shells only (readline, zsh and fish emacs mode; best effort under vi mode).
    /// PowerShell gets nothing: its PSReadLine binds Ctrl-U to a different edit.
    pub(super) const fn line_clear(self) -> &'static [u8] {
        match self {
            Self::Posix => b"\x05\x15",
            Self::PowerShell => b"",
        }
    }

    /// `text` as one single-quoted word, identical in bash, zsh, fish and nushell (and PowerShell);
    /// `None` when it holds a character that [`Self::unsafe_char`] refuses.
    fn quote(self, text: &str) -> Option<String> {
        if text.chars().any(|c| self.unsafe_char(c)) {
            return None;
        }
        Some(format!("'{text}'"))
    }

    /// Word as the shell should read it: bare when it only holds safe characters, else quoted.
    fn word(self, word: &str) -> Option<String> {
        if !word.is_empty() && word.chars().all(|c| self.bare_char(c)) {
            Some(word.to_owned())
        } else {
            self.quote(word)
        }
    }
}

/// The command line that starts Claude for the derived profile `preset` in `mode`: the
/// `CLAUDE_CONFIG_DIR` setting (only outside the default `~/.claude`), the preset's own binary and
/// args, then the mode flag. `None` when the dir, binary or an arg holds a character that cannot be quoted safely (the
/// caller shows the `Unquotable` notice).
#[must_use]
pub(super) fn quick_start_command(
    preset: &PresetConfig,
    mode: QuickStartMode,
    home: &Path,
    syntax: ShellSyntax,
) -> Option<String> {
    let config_dir = preset
        .env
        .get(CONFIG_DIR_ENV)
        .filter(|dir| Path::new(dir.as_str()) != default_claude_config_dir(home));
    let binary = preset.command.as_deref().unwrap_or(DEFAULT_BINARY);
    let mut words = Vec::new();
    if syntax == ShellSyntax::Posix
        && let Some(dir) = config_dir
    {
        words.push("env".to_owned());
        words.push(syntax.quote(&format!("{CONFIG_DIR_ENV}={dir}"))?);
    }
    let program = syntax.word(binary)?;
    if syntax == ShellSyntax::PowerShell && program != binary {
        // A quoted program name is a string expression until the call operator runs it.
        words.push("&".to_owned());
    }
    words.push(program);
    for arg in &preset.args {
        words.push(syntax.word(arg)?);
    }
    words.extend(mode.flag().map(str::to_owned));
    let line = words.join(" ");
    if syntax == ShellSyntax::PowerShell
        && let Some(dir) = config_dir
    {
        // The variable is process-wide in PowerShell: put the old value back afterwards.
        let quoted = syntax.quote(dir)?;
        return Some(format!(
            "$claudeDirBefore=$env:{CONFIG_DIR_ENV}; $env:{CONFIG_DIR_ENV}={quoted}; \
             try {{ {line} }} finally {{ $env:{CONFIG_DIR_ENV}=$claudeDirBefore }}"
        ));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use horizon_core::{PanelKind, PanelResume};

    use super::*;

    const HOME: &str = "/home/u";

    #[test]
    fn only_posix_shells_clear_a_half_typed_line() {
        assert_eq!(ShellSyntax::Posix.line_clear(), b"\x05\x15");
        assert!(ShellSyntax::PowerShell.line_clear().is_empty());
    }

    fn preset(dir: Option<&str>, command: Option<&str>, args: &[&str]) -> PresetConfig {
        PresetConfig {
            name: "Claude \u{b7} conta2".to_owned(),
            alias: Some("conta2".to_owned()),
            kind: PanelKind::Claude,
            command: command.map(str::to_owned),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
            env: dir
                .map(|dir| BTreeMap::from([(CONFIG_DIR_ENV.to_owned(), dir.to_owned())]))
                .unwrap_or_default(),
        }
    }

    fn command(preset: &PresetConfig, mode: QuickStartMode) -> Option<String> {
        quick_start_command(preset, mode, Path::new(HOME), ShellSyntax::Posix)
    }

    fn powershell(preset: &PresetConfig, mode: QuickStartMode) -> Option<String> {
        quick_start_command(preset, mode, Path::new(HOME), ShellSyntax::PowerShell)
    }

    #[test]
    fn other_profile_goes_through_env_so_fish_and_nushell_run_it_too() {
        let preset = preset(
            Some("/home/u/.claude-conta2"),
            Some("claude"),
            &["--dangerously-skip-permissions"],
        );

        assert_eq!(
            command(&preset, QuickStartMode::Continue).as_deref(),
            Some("env 'CLAUDE_CONFIG_DIR=/home/u/.claude-conta2' claude --dangerously-skip-permissions --continue")
        );
    }

    #[test]
    fn default_config_dir_omits_the_prefix_even_with_a_trailing_slash() {
        for dir in ["/home/u/.claude", "/home/u/.claude/"] {
            let preset = preset(Some(dir), Some("claude"), &[]);

            assert_eq!(
                command(&preset, QuickStartMode::New).as_deref(),
                Some("claude"),
                "{dir}"
            );
        }
        assert_eq!(
            command(&preset(None, None, &[]), QuickStartMode::New).as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn modes_add_exactly_their_own_flag_last() {
        let preset = preset(None, Some("claude"), &["--model", "opus"]);

        assert_eq!(
            command(&preset, QuickStartMode::New).as_deref(),
            Some("claude --model opus")
        );
        assert_eq!(
            command(&preset, QuickStartMode::Continue).as_deref(),
            Some("claude --model opus --continue")
        );
        assert_eq!(
            command(&preset, QuickStartMode::Resume).as_deref(),
            Some("claude --model opus --resume")
        );
    }

    #[test]
    fn config_dir_with_spaces_and_shell_metacharacters_stays_inside_the_quotes() {
        let preset = preset(Some("/tmp/my dir $(reboot);x"), Some("claude"), &[]);

        assert_eq!(
            command(&preset, QuickStartMode::New).as_deref(),
            Some("env 'CLAUDE_CONFIG_DIR=/tmp/my dir $(reboot);x' claude")
        );
    }

    #[test]
    fn config_dir_with_quote_backslash_or_newline_is_refused() {
        for dir in ["/tmp/it's", "/tmp/a\\b", "/tmp/a\nb", "/tmp/a\0b"] {
            assert_eq!(
                command(&preset(Some(dir), Some("claude"), &[]), QuickStartMode::New),
                None,
                "{dir:?}"
            );
        }
    }

    #[test]
    fn binary_and_args_with_quote_or_backslash_are_refused() {
        assert_eq!(
            command(&preset(None, Some("my'claude"), &[]), QuickStartMode::New),
            None
        );
        assert_eq!(
            command(&preset(None, Some("c\\claude"), &[]), QuickStartMode::New),
            None
        );
        assert_eq!(
            command(&preset(None, None, &["--p", "it's"]), QuickStartMode::New),
            None
        );
        assert_eq!(command(&preset(None, None, &["a\\nb"]), QuickStartMode::New), None);
    }

    #[test]
    fn control_characters_in_dir_binary_or_args_refuse_the_command() {
        assert_eq!(
            command(&preset(Some("/tmp/a\nrm -rf /"), None, &[]), QuickStartMode::New),
            None
        );
        assert_eq!(command(&preset(None, Some("claude\r"), &[]), QuickStartMode::New), None);
        assert_eq!(command(&preset(None, None, &["--x\n"]), QuickStartMode::New), None);
        assert_eq!(
            powershell(&preset(Some("/tmp/a\nb"), None, &[]), QuickStartMode::New),
            None
        );
        assert_eq!(powershell(&preset(None, None, &["--x\n"]), QuickStartMode::New), None);
    }

    #[test]
    fn awkward_binary_and_args_are_quoted_not_dropped() {
        let preset = preset(
            None,
            Some("/opt/my tools/claude"),
            &["--append-system-prompt", "be brief; ok", ""],
        );

        assert_eq!(
            command(&preset, QuickStartMode::New).as_deref(),
            Some("'/opt/my tools/claude' --append-system-prompt 'be brief; ok' ''")
        );
    }

    #[test]
    fn powershell_sets_the_variable_for_the_run_only_and_calls_a_quoted_program() {
        let plain = preset(None, Some("claude"), &["--model", "opus"]);
        let other = preset(
            Some("C:\\Users\\u\\.claude-conta2"),
            Some("C:\\Program Files\\claude.exe"),
            &[],
        );

        assert_eq!(
            powershell(&plain, QuickStartMode::Resume).as_deref(),
            Some("claude --model opus --resume")
        );
        assert_eq!(
            powershell(&other, QuickStartMode::New).as_deref(),
            Some(
                "$claudeDirBefore=$env:CLAUDE_CONFIG_DIR; $env:CLAUDE_CONFIG_DIR='C:\\Users\\u\\.claude-conta2'; \
                 try { & 'C:\\Program Files\\claude.exe' } finally { $env:CLAUDE_CONFIG_DIR=$claudeDirBefore }"
            )
        );
    }

    #[test]
    fn powershell_refuses_quote_glyphs_backtick_and_dollar_and_keeps_commas_out_of_bare_words() {
        for arg in ["it's", "\u{2019}x\u{2019}", "a`b", "$env:X"] {
            assert_eq!(
                powershell(&preset(None, Some("claude"), &[arg]), QuickStartMode::New),
                None,
                "{arg}"
            );
        }
        assert_eq!(
            powershell(&preset(None, Some("claude"), &["a,b"]), QuickStartMode::New).as_deref(),
            Some("claude 'a,b'")
        );
    }
}
