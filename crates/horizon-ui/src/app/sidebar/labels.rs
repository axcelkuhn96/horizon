//! Sidebar panel labels: the one-line text of a panel row and its dim
//! right-aligned hint.
//!
//! Label rules (first match wins): the user's own panel name; `Claude · <profile>`
//! (plain `Claude Code` for the default profile) for Claude panels; the terminal
//! title with the `user@host` prefix stripped and `$HOME` shown as `~`; the kind
//! name. SSH panels keep `Panel::display_title()` as is (host identity included).
//! Strings and galleys are built once per change (see [`PanelLabelCache`]),
//! never per frame.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use egui::{Color32, FontId, Galley, Painter};
use horizon_core::{PanelId, PanelKind, PresetConfig};

use super::git_overview::middle_ellipsis;
use crate::app::hub_ui;

/// Paths longer than this are cut in the middle so the leaf directory stays visible.
const PATH_MAX_CHARS: usize = 34;
const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";
const DEFAULT_CLAUDE_LABEL: &str = "Claude Code";
const LABEL_FONT_SIZE: f32 = 12.0;
const HINT_FONT_SIZE: f32 = 10.5;

/// Which Claude profile a panel runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProfileRef<'a> {
    /// Not a Claude profile panel.
    None,
    /// A Claude panel on the default config dir.
    Default,
    /// A Claude panel on the named profile.
    Named(&'a str),
}

/// Everything the label of one panel depends on.
#[derive(Clone, Copy, Debug)]
pub(super) struct LabelInputs<'a> {
    pub(super) title: &'a str,
    pub(super) terminal_title: &'a str,
    pub(super) custom_name: bool,
    pub(super) kind: PanelKind,
    pub(super) profile: ProfileRef<'a>,
    pub(super) launch_command: Option<&'a str>,
    /// `Panel::display_title()` of an SSH panel (host identity kept); empty for other kinds.
    pub(super) display_title: &'a str,
}

// ---------------------------------------------------------------------------
// Pure rules
// ---------------------------------------------------------------------------

fn normalize_config_dir(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.len() > 1 {
        trimmed.trim_end_matches('/')
    } else {
        trimmed
    }
}

/// Profile of a panel from its `CLAUDE_CONFIG_DIR`: the alias of the derived
/// Claude preset carrying the same dir, [`ProfileRef::Default`] for a Claude
/// panel without one, [`ProfileRef::None`] otherwise.
#[must_use]
pub(super) fn claude_profile_for<'a>(
    kind: PanelKind,
    env: &std::collections::BTreeMap<String, String>,
    presets: &'a [PresetConfig],
) -> ProfileRef<'a> {
    let dir = env
        .get(CONFIG_DIR_ENV)
        .map(|raw| normalize_config_dir(raw))
        .filter(|dir| !dir.is_empty());
    let Some(dir) = dir else {
        return if kind == PanelKind::Claude {
            ProfileRef::Default
        } else {
            ProfileRef::None
        };
    };
    presets
        .iter()
        .filter(|preset| preset.kind == PanelKind::Claude)
        .find(|preset| {
            preset
                .env
                .get(CONFIG_DIR_ENV)
                .is_some_and(|candidate| normalize_config_dir(candidate) == dir)
        })
        .and_then(|preset| preset.alias.as_deref())
        .map_or(ProfileRef::None, ProfileRef::Named)
}

/// Splits `user@host` identity text off the front of a terminal title.
/// Recognised separators: `user@host: rest`, `user@host — rest`, `user@host - rest`.
/// A leading generic label (`shell — user@host: rest`) is skipped too.
/// `None` when the title carries no such prefix; the rest can be empty.
fn strip_identity_prefix(title: &str) -> Option<&str> {
    let title = title.trim();
    if let Some(rest) = strip_identity_at_start(title) {
        return Some(rest);
    }
    let (_, after) = title.split_once(" \u{2014} ")?;
    strip_identity_at_start(after.trim_start())
}

fn strip_identity_at_start(text: &str) -> Option<&str> {
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    let token = &text[..end];
    let (identity, separator_in_token) = match token.find(':') {
        Some(colon) => (&token[..colon], true),
        None => (token, false),
    };
    let (user, host) = identity.split_once('@')?;
    if user.is_empty() || host.is_empty() || host.contains('@') {
        return None;
    }
    let after = if separator_in_token {
        &text[identity.len() + 1..]
    } else {
        let rest = text[end..].trim_start();
        rest.strip_prefix('\u{2014}')
            .or_else(|| rest.strip_prefix('-'))
            .or_else(|| rest.strip_prefix(':'))
            .unwrap_or(rest)
    };
    Some(after.trim())
}

/// `$HOME` (and `$HOME/…`) written as `~` (`~/…`).
fn shorten_home(path: &str, home: Option<&str>) -> String {
    let Some(home) = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty())
    else {
        return path.to_owned();
    };
    match path.strip_prefix(home) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

fn looks_like_path(text: &str) -> bool {
    text == "~" || text.starts_with('/') || text.starts_with("~/")
}

/// Terminal title as a row label: identity prefix stripped, home shortened,
/// long paths cut in the middle. `None` when nothing useful remains.
#[must_use]
fn clean_terminal_title(title: &str, home: Option<&str>) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let body = match strip_identity_prefix(title) {
        Some("") => return None,
        Some(rest) => rest,
        None => title,
    };
    let shortened = shorten_home(body, home);
    Some(if looks_like_path(&shortened) {
        middle_ellipsis(&shortened, PATH_MAX_CHARS)
    } else {
        shortened
    })
}

/// SSH panels are labelled by `Panel::display_title()`, host identity included.
fn ssh_title<'a>(inputs: &LabelInputs<'a>) -> Option<&'a str> {
    let title = inputs.display_title.trim();
    (inputs.kind == PanelKind::Ssh && !title.is_empty()).then_some(title)
}

/// Whether the user chose the panel name. A preset launched as "shell" carries
/// that name without the user having chosen it.
fn has_user_name(inputs: &LabelInputs<'_>) -> bool {
    let name = inputs.title.trim();
    inputs.custom_name && !name.is_empty() && !name.eq_ignore_ascii_case(inputs.kind.display_name())
}

/// The label of a panel row.
#[must_use]
pub(super) fn smart_label(inputs: &LabelInputs<'_>, home: Option<&str>) -> String {
    if let Some(title) = ssh_title(inputs) {
        return title.to_owned();
    }
    if has_user_name(inputs) {
        return inputs.title.trim().to_owned();
    }
    match inputs.profile {
        ProfileRef::Named(name) => return format!("Claude \u{b7} {name}"),
        ProfileRef::Default => return DEFAULT_CLAUDE_LABEL.to_owned(),
        ProfileRef::None => {}
    }
    clean_terminal_title(inputs.terminal_title, home)
        .or_else(|| clean_terminal_title(inputs.title, home))
        .unwrap_or_else(|| inputs.kind.display_name().to_owned())
}

/// Program name of a shell or command panel (`/usr/bin/zsh` -> `zsh`).
#[must_use]
pub(super) fn program_hint(
    kind: PanelKind,
    launch_command: Option<&str>,
    default_shell: Option<&str>,
) -> Option<String> {
    let program = match kind {
        PanelKind::Shell => launch_command.or(default_shell)?,
        PanelKind::Command => launch_command?,
        _ => return None,
    };
    let first = program.split_whitespace().next()?;
    let name = Path::new(first).file_name()?.to_str()?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// `7d 34%`; the percentage is rounded and clamped to 0..=999.
#[must_use]
pub(super) fn usage_hint(percent: f32) -> String {
    format!("7d {}%", percent_key(percent))
}

/// Whole-percent cache key of a usage reading.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn percent_key(percent: f32) -> u32 {
    percent.round().clamp(0.0, 999.0) as u32
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// The inputs the label actually depends on; every other field is blanked, so
/// a change in an unused one (an animated terminal title on a Claude or named
/// panel) neither misses nor rebuilds the cache.
fn effective<'a>(inputs: &LabelInputs<'a>) -> LabelInputs<'a> {
    let base = LabelInputs {
        title: "",
        terminal_title: "",
        custom_name: false,
        profile: ProfileRef::None,
        display_title: "",
        ..*inputs
    };
    if ssh_title(inputs).is_some() {
        return LabelInputs {
            display_title: inputs.display_title,
            ..base
        };
    }
    if has_user_name(inputs) {
        return LabelInputs {
            title: inputs.title,
            custom_name: true,
            ..base
        };
    }
    if inputs.profile != ProfileRef::None {
        return LabelInputs {
            profile: inputs.profile,
            ..base
        };
    }
    LabelInputs {
        title: inputs.title,
        terminal_title: inputs.terminal_title,
        ..base
    }
}

/// Owned copy of the [`effective`] [`LabelInputs`] the cached label was built from.
struct StoredInputs {
    title: String,
    terminal_title: String,
    custom_name: bool,
    kind: PanelKind,
    /// `None` = not a profile panel, `Some(None)` = default, `Some(Some(name))` = named.
    profile: Option<Option<String>>,
    launch_command: Option<String>,
    display_title: String,
}

impl StoredInputs {
    fn from_inputs(inputs: &LabelInputs<'_>) -> Self {
        Self {
            title: inputs.title.to_owned(),
            terminal_title: inputs.terminal_title.to_owned(),
            custom_name: inputs.custom_name,
            kind: inputs.kind,
            profile: match inputs.profile {
                ProfileRef::None => None,
                ProfileRef::Default => Some(None),
                ProfileRef::Named(name) => Some(Some(name.to_owned())),
            },
            launch_command: inputs.launch_command.map(str::to_owned),
            display_title: inputs.display_title.to_owned(),
        }
    }

    /// `inputs` must be [`effective`] inputs.
    fn same_as(&self, inputs: &LabelInputs<'_>) -> bool {
        let profile_same = match (&self.profile, inputs.profile) {
            (None, ProfileRef::None) | (Some(None), ProfileRef::Default) => true,
            (Some(Some(stored)), ProfileRef::Named(name)) => stored == name,
            _ => false,
        };
        profile_same
            && self.custom_name == inputs.custom_name
            && self.kind == inputs.kind
            && self.title == inputs.title
            && self.terminal_title == inputs.terminal_title
            && self.launch_command.as_deref() == inputs.launch_command
            && self.display_title == inputs.display_title
    }
}

/// Galley laid out for a pixels-per-point and a width.
struct CachedGalley {
    ppp_bits: u32,
    width_bits: u32,
    galley: Arc<Galley>,
}

struct LabelEntry {
    inputs: StoredInputs,
    label: Arc<str>,
    /// Program hint (`zsh`); static for the panel.
    program: Option<Arc<str>>,
    /// Rounded 7d usage the usage hint was built for.
    usage_key: Option<u32>,
    usage: Option<Arc<str>>,
    label_galley: Option<CachedGalley>,
    hint_galley: Option<CachedGalley>,
    generation: u64,
}

/// Row text of one panel as the row painter needs it. The dim right-aligned
/// hint (`7d 34%` for Claude profile panels, else the program name) is read
/// through [`PanelLabelCache::hint_galley`].
#[derive(Clone)]
pub(super) struct ResolvedLabel {
    pub(super) label: Arc<str>,
}

/// Labels and galleys of the sidebar panel rows, rebuilt only when a panel's
/// label inputs (or the width / scale) change.
#[derive(Default)]
pub(in crate::app) struct PanelLabelCache {
    entries: HashMap<PanelId, LabelEntry>,
    home: Option<String>,
    default_shell: Option<String>,
    env_read: bool,
    generation: u64,
}

impl PanelLabelCache {
    /// Marks a new frame; entries not resolved before [`Self::end_frame`] are dropped there.
    pub(super) fn begin_frame(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if !self.env_read {
            self.home = std::env::var("HOME").ok();
            self.default_shell = std::env::var("SHELL").ok();
            self.env_read = true;
        }
    }

    /// Forgets the panels that were not resolved this frame.
    pub(super) fn end_frame(&mut self) {
        let generation = self.generation;
        self.entries.retain(|_, entry| entry.generation == generation);
    }

    /// Label of `id` (and its hint, read via [`Self::hint_galley`]); rebuilt only
    /// when the inputs the label depends on or the usage reading changed.
    pub(super) fn resolve(&mut self, id: PanelId, inputs: &LabelInputs<'_>, seven_day: Option<f32>) -> ResolvedLabel {
        let generation = self.generation;
        let key = effective(inputs);
        let stale = self.entries.get(&id).is_none_or(|entry| !entry.inputs.same_as(&key));
        if stale {
            let label: Arc<str> = smart_label(inputs, self.home.as_deref()).into();
            let program =
                program_hint(inputs.kind, inputs.launch_command, self.default_shell.as_deref()).map(Arc::from);
            self.entries.insert(
                id,
                LabelEntry {
                    inputs: StoredInputs::from_inputs(&key),
                    label,
                    program,
                    usage_key: None,
                    usage: None,
                    label_galley: None,
                    hint_galley: None,
                    generation,
                },
            );
        }
        let Some(entry) = self.entries.get_mut(&id) else {
            return ResolvedLabel { label: Arc::from("") };
        };
        entry.generation = generation;
        let wants_usage = matches!(inputs.profile, ProfileRef::Named(_));
        let key = seven_day.filter(|_| wants_usage).map(percent_key);
        if entry.usage_key != key || (key.is_some() && entry.usage.is_none()) {
            entry.usage_key = key;
            entry.usage = seven_day
                .filter(|_| wants_usage)
                .map(|percent| Arc::from(usage_hint(percent)));
            entry.hint_galley = None;
        }
        ResolvedLabel {
            label: Arc::clone(&entry.label),
        }
    }

    /// Hint text of `id` as the row shows it.
    #[cfg(test)]
    fn hint_text(&self, id: PanelId) -> Option<Arc<str>> {
        let entry = self.entries.get(&id)?;
        entry.usage.clone().or_else(|| entry.program.clone())
    }

    /// Galley of the hint of `id`, laid out again only when the scale moved.
    pub(super) fn hint_galley(&mut self, id: PanelId, painter: &Painter, ppp: f32) -> Option<Arc<Galley>> {
        let entry = self.entries.get_mut(&id)?;
        let text = entry.usage.clone().or_else(|| entry.program.clone())?;
        let ppp_bits = ppp.to_bits();
        if !entry
            .hint_galley
            .as_ref()
            .is_some_and(|cached| cached.ppp_bits == ppp_bits)
        {
            entry.hint_galley = Some(CachedGalley {
                ppp_bits,
                width_bits: 0,
                galley: painter.layout_no_wrap(
                    text.to_string(),
                    FontId::proportional(HINT_FONT_SIZE),
                    Color32::PLACEHOLDER,
                ),
            });
        }
        entry.hint_galley.as_ref().map(|cached| Arc::clone(&cached.galley))
    }

    /// Galley of the label of `id` elided at `width`, laid out again only when
    /// the scale or the width moved.
    pub(super) fn label_galley(&mut self, id: PanelId, painter: &Painter, ppp: f32, width: f32) -> Option<Arc<Galley>> {
        let entry = self.entries.get_mut(&id)?;
        let ppp_bits = ppp.to_bits();
        let width_bits = width.round().to_bits();
        if !entry
            .label_galley
            .as_ref()
            .is_some_and(|cached| cached.ppp_bits == ppp_bits && cached.width_bits == width_bits)
        {
            entry.label_galley = Some(CachedGalley {
                ppp_bits,
                width_bits,
                galley: hub_ui::elided(painter, &entry.label, FontId::proportional(LABEL_FONT_SIZE), width),
            });
        }
        entry.label_galley.as_ref().map(|cached| Arc::clone(&cached.galley))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const HOME: Option<&str> = Some("/home/axcel");

    fn inputs<'a>(title: &'a str, terminal_title: &'a str, kind: PanelKind) -> LabelInputs<'a> {
        LabelInputs {
            title,
            terminal_title,
            custom_name: false,
            kind,
            profile: ProfileRef::None,
            launch_command: None,
            display_title: "",
        }
    }

    #[test]
    fn custom_name_beats_everything() {
        let mut named = inputs("Backend", "axcel@box: ~/expia", PanelKind::Shell);
        named.custom_name = true;
        named.profile = ProfileRef::Named("claude2");
        assert_eq!(smart_label(&named, HOME), "Backend");
    }

    #[test]
    fn preset_name_equal_to_the_kind_is_not_a_custom_name() {
        let mut preset = inputs("shell", "axcel@box: /home/axcel/expia", PanelKind::Shell);
        preset.custom_name = true;
        assert_eq!(smart_label(&preset, HOME), "~/expia");
    }

    #[test]
    fn claude_profile_panels_read_claude_dot_profile() {
        let mut panel = inputs("Claude", "✳ Task", PanelKind::Claude);
        panel.profile = ProfileRef::Named("claude2");
        assert_eq!(smart_label(&panel, HOME), "Claude \u{b7} claude2");
    }

    #[test]
    fn default_claude_panel_is_plain_claude_code() {
        let mut panel = inputs("Claude", "", PanelKind::Claude);
        panel.profile = ProfileRef::Default;
        assert_eq!(smart_label(&panel, HOME), "Claude Code");
    }

    #[test]
    fn terminal_title_loses_user_host_prefix_and_shortens_home() {
        let panel = inputs("shell", "axcel@axcel-Nitro: /home/axcel/expia", PanelKind::Shell);
        assert_eq!(smart_label(&panel, HOME), "~/expia");
    }

    #[test]
    fn em_dash_and_leading_label_forms_are_stripped() {
        let dash = inputs("shell", "axcel@box \u{2014} /home/axcel/api", PanelKind::Shell);
        assert_eq!(smart_label(&dash, HOME), "~/api");
        let labelled = inputs("shell", "shell \u{2014} axcel@box: ~/api", PanelKind::Shell);
        assert_eq!(smart_label(&labelled, HOME), "~/api");
    }

    #[test]
    fn bare_home_directory_is_a_tilde() {
        let panel = inputs("shell", "axcel@box: /home/axcel", PanelKind::Shell);
        assert_eq!(smart_label(&panel, HOME), "~");
    }

    #[test]
    fn home_prefix_needs_a_path_boundary() {
        assert_eq!(shorten_home("/home/axcelito/x", HOME), "/home/axcelito/x");
        assert_eq!(shorten_home("/home/axcel/x", HOME), "~/x");
        assert_eq!(shorten_home("/home/axcel/x", None), "/home/axcel/x");
    }

    #[test]
    fn long_paths_are_cut_in_the_middle_keeping_the_leaf() {
        let panel = inputs(
            "shell",
            "axcel@box: ~/projects/clients/acme/services/billing-api/src",
            PanelKind::Shell,
        );
        let label = smart_label(&panel, HOME);
        assert!(label.chars().count() <= PATH_MAX_CHARS, "{label}");
        assert!(label.contains('\u{2026}'), "{label}");
        assert!(label.ends_with("billing-api/src"), "{label}");
        assert!(label.starts_with("~/"), "{label}");
    }

    #[test]
    fn plain_titles_pass_through_and_empty_falls_back_to_kind() {
        assert_eq!(
            smart_label(&inputs("x", "vim main.rs", PanelKind::Shell), HOME),
            "vim main.rs"
        );
        assert_eq!(
            smart_label(&inputs("Shell", "", PanelKind::FileExplorer), HOME),
            "Shell"
        );
        assert_eq!(smart_label(&inputs("", "", PanelKind::FileExplorer), HOME), "Files");
    }

    #[test]
    fn identity_only_title_is_not_used() {
        let panel = inputs("", "axcel@box", PanelKind::Shell);
        assert_eq!(smart_label(&panel, HOME), "Shell");
    }

    #[test]
    fn mail_like_titles_are_left_alone() {
        assert_eq!(strip_identity_prefix("mail a@b.com about it"), None);
        assert_eq!(strip_identity_prefix("not an identity"), None);
    }

    fn profile_preset(alias: &str, dir: &str) -> PresetConfig {
        let mut preset = PresetConfig {
            name: format!("Claude {alias}"),
            alias: Some(alias.to_owned()),
            kind: PanelKind::Claude,
            command: None,
            args: Vec::new(),
            resume: horizon_core::PanelResume::default(),
            ssh_connection: None,
            env: BTreeMap::new(),
        };
        preset.env.insert(CONFIG_DIR_ENV.to_owned(), dir.to_owned());
        preset
    }

    #[test]
    fn profile_matches_by_normalised_config_dir() {
        let presets = vec![
            profile_preset("claude2", "/home/u/.claude2"),
            profile_preset("work", "~/.work"),
        ];
        let mut env = BTreeMap::new();
        env.insert(CONFIG_DIR_ENV.to_owned(), "/home/u/.claude2/".to_owned());
        assert_eq!(
            claude_profile_for(PanelKind::Claude, &env, &presets),
            ProfileRef::Named("claude2")
        );
        // Shell panels carrying a profile dir are profile panels too.
        assert_eq!(
            claude_profile_for(PanelKind::Shell, &env, &presets),
            ProfileRef::Named("claude2")
        );
    }

    #[test]
    fn profile_defaults_and_unknown_dirs() {
        let presets = vec![profile_preset("claude2", "/home/u/.claude2")];
        let empty = BTreeMap::new();
        assert_eq!(
            claude_profile_for(PanelKind::Claude, &empty, &presets),
            ProfileRef::Default
        );
        assert_eq!(claude_profile_for(PanelKind::Shell, &empty, &presets), ProfileRef::None);
        let mut other = BTreeMap::new();
        other.insert(CONFIG_DIR_ENV.to_owned(), "/elsewhere".to_owned());
        assert_eq!(
            claude_profile_for(PanelKind::Claude, &other, &presets),
            ProfileRef::None
        );
    }

    #[test]
    fn program_hint_names_the_shell_or_command() {
        assert_eq!(
            program_hint(PanelKind::Shell, None, Some("/usr/bin/zsh")),
            Some("zsh".to_owned())
        );
        assert_eq!(
            program_hint(PanelKind::Shell, Some("/bin/fish -l"), Some("/usr/bin/zsh")),
            Some("fish".to_owned())
        );
        assert_eq!(
            program_hint(PanelKind::Command, Some("htop"), None),
            Some("htop".to_owned())
        );
        assert_eq!(program_hint(PanelKind::Command, None, Some("/bin/zsh")), None);
        assert_eq!(program_hint(PanelKind::Claude, None, Some("/bin/zsh")), None);
    }

    #[test]
    fn usage_hint_rounds_and_clamps() {
        assert_eq!(usage_hint(33.6), "7d 34%");
        assert_eq!(usage_hint(-3.0), "7d 0%");
    }

    #[test]
    fn cache_rebuilds_only_when_inputs_change() {
        let mut cache = PanelLabelCache {
            home: HOME.map(str::to_owned),
            env_read: true,
            ..PanelLabelCache::default()
        };
        let id = PanelId(7);
        let first = cache.resolve(id, &inputs("shell", "axcel@box: ~/a", PanelKind::Shell), None);
        let again = cache.resolve(id, &inputs("shell", "axcel@box: ~/a", PanelKind::Shell), None);
        assert!(
            Arc::ptr_eq(&first.label, &again.label),
            "unchanged inputs reuse the label"
        );
        let moved = cache.resolve(id, &inputs("shell", "axcel@box: ~/b", PanelKind::Shell), None);
        assert_eq!(&*moved.label, "~/b");
    }

    #[test]
    fn usage_hint_follows_the_reading_for_profile_panels_only() {
        let mut cache = PanelLabelCache {
            env_read: true,
            ..PanelLabelCache::default()
        };
        let id = PanelId(1);
        let mut panel = inputs("Claude", "", PanelKind::Claude);
        panel.profile = ProfileRef::Named("claude2");
        let _ = cache.resolve(id, &panel, None);
        assert_eq!(cache.hint_text(id), None);
        let _ = cache.resolve(id, &panel, Some(33.6));
        let hint = cache.hint_text(id);
        assert_eq!(hint.as_deref(), Some("7d 34%"));
        let _ = cache.resolve(id, &panel, Some(34.2));
        let same = cache.hint_text(id);
        assert!(Arc::ptr_eq(hint.as_ref().expect("hint"), same.as_ref().expect("hint")));
        let shell = inputs("shell", "", PanelKind::Shell);
        let _ = cache.resolve(PanelId(2), &shell, Some(50.0));
        assert_eq!(cache.hint_text(PanelId(2)), None);
    }

    #[test]
    fn end_frame_drops_unresolved_panels() {
        let mut cache = PanelLabelCache::default();
        cache.begin_frame();
        let _ = cache.resolve(PanelId(1), &inputs("a", "", PanelKind::Shell), None);
        let _ = cache.resolve(PanelId(2), &inputs("b", "", PanelKind::Shell), None);
        cache.end_frame();
        cache.begin_frame();
        let _ = cache.resolve(PanelId(1), &inputs("a", "", PanelKind::Shell), None);
        cache.end_frame();
        assert!(cache.entries.contains_key(&PanelId(1)));
        assert!(!cache.entries.contains_key(&PanelId(2)));
    }

    #[test]
    fn ssh_panels_keep_the_host_identity() {
        let mut panel = inputs("Prod API", "deploy@prod-api: ~/srv", PanelKind::Ssh);
        panel.custom_name = true;
        panel.display_title = "Prod API (deploy@prod-api) \u{2014} deploy@prod-api: ~/srv";
        assert_eq!(
            smart_label(&panel, HOME),
            "Prod API (deploy@prod-api) \u{2014} deploy@prod-api: ~/srv"
        );
        let mut unnamed = inputs("deploy@prod-api", "deploy@prod-api: ~/srv", PanelKind::Ssh);
        unnamed.display_title = "deploy@prod-api: ~/srv";
        assert_eq!(smart_label(&unnamed, HOME), "deploy@prod-api: ~/srv");
        // Without a display title the generic rules apply.
        unnamed.display_title = "";
        assert_eq!(smart_label(&unnamed, HOME), "~/srv");
    }

    #[test]
    fn animated_terminal_titles_do_not_rebuild_claude_or_named_panels() {
        let mut cache = PanelLabelCache {
            env_read: true,
            ..PanelLabelCache::default()
        };
        let mut claude = inputs("Claude", "\u{2733} Task one", PanelKind::Claude);
        claude.profile = ProfileRef::Named("claude2");
        let first = cache.resolve(PanelId(1), &claude, None);
        claude.terminal_title = "\u{2736} Task two";
        let second = cache.resolve(PanelId(1), &claude, None);
        assert!(Arc::ptr_eq(&first.label, &second.label));

        let mut named = inputs("Backend", "build 1", PanelKind::Shell);
        named.custom_name = true;
        let first = cache.resolve(PanelId(2), &named, None);
        named.terminal_title = "build 2";
        let second = cache.resolve(PanelId(2), &named, None);
        assert!(Arc::ptr_eq(&first.label, &second.label));
        named.title = "Frontend";
        assert_eq!(&*cache.resolve(PanelId(2), &named, None).label, "Frontend");

        // A plain shell does follow its terminal title.
        let shell = inputs("shell", "axcel@box: ~/a", PanelKind::Shell);
        let _ = cache.resolve(PanelId(3), &shell, None);
        let moved = inputs("shell", "axcel@box: ~/b", PanelKind::Shell);
        assert_eq!(&*cache.resolve(PanelId(3), &moved, None).label, "~/b");
    }

    #[test]
    fn label_galley_reports_elision() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let mut cache = PanelLabelCache {
                env_read: true,
                ..PanelLabelCache::default()
            };
            let id = PanelId(1);
            let _ = cache.resolve(
                id,
                &inputs(
                    "x",
                    "a long terminal title that cannot fit in forty points",
                    PanelKind::Shell,
                ),
                None,
            );
            let narrow = cache.label_galley(id, &painter, 1.0, 40.0).expect("galley");
            assert!(narrow.elided);
            let wide = cache.label_galley(id, &painter, 1.0, 4000.0).expect("galley");
            assert!(!wide.elided);
        });
    }
}
