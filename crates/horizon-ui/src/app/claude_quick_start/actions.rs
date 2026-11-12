//! What the strip does besides drawing: availability of the button, building and refreshing the
//! profile rows, applying a [`StripEvent`] and typing the start command into the panel's PTY.

use std::time::Instant;

use egui::{Color32, Context};
use horizon_core::link::{LinkRequest, LinkResponse};
use horizon_core::{PanelId, PanelKind, PromptScope};

use crate::app::HorizonApp;
use crate::app::claude_auto::MIN_PROFILES_FOR_AUTO;
use crate::app::claude_launcher::{RowUsage, claude_profile_names, find_profile_preset, is_profile_preset};
use crate::app::hub_ui;
use crate::theme;

use super::command::{ShellSyntax, quick_start_command};
use super::state::{Expanded, Row};
use super::titlebar;
use super::{
    Availability, PromptInsert, QuickStartMode, Stage, StartFailure, StartRequest, StripEvent, quick_start_visible,
};

impl HorizonApp {
    /// Titlebar button state for `panel_id`: `None` hides it. The shell is probed for a running
    /// foreground job only while the pointer is over the titlebar or the strip is open.
    pub(super) fn quick_start_button(
        &mut self,
        panel_id: PanelId,
        panel_width: f32,
        hovered: bool,
    ) -> Option<Availability> {
        if !titlebar::panel_is_wide_enough(panel_width) {
            return None;
        }
        let panel = self.board.panel(panel_id)?;
        let is_ssh = panel.kind == PanelKind::Ssh || panel.ssh_connection.is_some();
        let profiles = self.presets.iter().filter(|preset| is_profile_preset(preset)).count();
        if !quick_start_visible(panel.kind, is_ssh, profiles) {
            return None;
        }
        let (kind, child_pid, exited) = (panel.kind, panel.child_pid(), panel.child_exited());
        let may_probe = hovered || self.hub.quick_start.is_open_for(panel_id);
        let busy = !exited
            && self
                .hub
                .quick_start
                .busy
                .is_busy(panel_id, Instant::now(), may_probe, || {
                    child_pid.is_some_and(titlebar::shell_is_busy)
                });
        Some(Availability::of(kind, is_ssh, busy, exited))
    }

    fn build_quick_rows(&mut self, names: Vec<String>) -> Vec<Row> {
        let with_auto = names.len() >= MIN_PROFILES_FOR_AUTO;
        let accents: Vec<Color32> = self
            .hub
            .accent_slots(&names)
            .iter()
            .map(|slot| hub_ui::accent_color(*slot))
            .collect();
        let mut rows: Vec<Row> = names
            .into_iter()
            .zip(accents)
            .map(|(name, accent)| {
                let usage = RowUsage::new(self.hub.usage.usage(&name));
                let email = self.hub.usage.account_email(&name);
                Row::new(name, accent, email, &usage)
            })
            .collect();
        if with_auto {
            rows.insert(0, Row::auto(&self.auto_choice()));
        }
        rows
    }

    /// One row per saved prompt of `panel_id`'s workspace: the workspace ones in the accent colour,
    /// the global ones dim; the tooltip shows the scope and the first line of the text.
    fn build_prompt_rows(&mut self, panel_id: PanelId) -> Vec<Row> {
        self.hub
            .prompts
            .refresh_for(&self.template_config, &self.board, Some(panel_id));
        self.hub
            .prompts
            .effective()
            .iter()
            .map(|prompt| Row {
                name: prompt.name.clone(),
                accent: if prompt.scope == PromptScope::Workspace {
                    theme::ACCENT()
                } else {
                    theme::FG_DIM()
                },
                account: None,
                dim: false,
                tooltip: format!(
                    "{}\n{}",
                    prompt.scope.label(),
                    crate::app::prompts::first_line(&prompt.text)
                ),
                auto: false,
            })
            .collect()
    }

    fn open_quick_start_strip(&mut self, panel_id: PanelId) {
        let names = claude_profile_names(&self.presets);
        if names.is_empty() {
            return;
        }
        let rows = self.build_quick_rows(names);
        let prompts = self.build_prompt_rows(panel_id);
        let mut expanded = Expanded::new(panel_id, rows, self.hub.rows_version);
        expanded.set_prompts(prompts);
        self.hub.quick_start.expanded = Some(expanded);
    }

    /// Opens the strip of `panel_id` at its accounts (the profile list), or brings an open one
    /// back to it. The account badge's click.
    pub(in crate::app) fn open_quick_start_profiles(&mut self, panel_id: PanelId) {
        if self.hub.quick_start.is_open_for(panel_id) {
            if let Some(expanded) = self.hub.quick_start.expanded.as_mut() {
                expanded.stage = Stage::Profiles;
            }
        } else {
            self.open_quick_start_strip(panel_id);
        }
    }

    /// Rebuilds the open strip's rows when the profile list or the usage data moved.
    pub(super) fn refresh_quick_start_rows(&mut self, panel_id: PanelId) {
        let version = self.hub.rows_version;
        let (current, prompt_stage) = match self
            .hub
            .quick_start
            .expanded
            .as_ref()
            .filter(|expanded| expanded.panel_id == panel_id && expanded.rows_version != version)
        {
            None => return,
            Some(expanded) => (
                match expanded.stage {
                    Stage::Modes { profile } => expanded.rows.get(profile).map(|row| (row.name.clone(), row.auto)),
                    Stage::Collapsed | Stage::Profiles | Stage::Prompts | Stage::PromptModes { .. } => None,
                },
                // The prompt pages list no profiles, so a usage refresh must not close them.
                expanded.stage.is_prompt_stage().then_some(expanded.stage),
            ),
        };
        let names = claude_profile_names(&self.presets);
        if names.is_empty() {
            self.hub.quick_start.expanded = None;
            return;
        }
        let rows = self.build_quick_rows(names);
        let remembered =
            current.and_then(|(name, auto)| rows.iter().position(|row| row.name == name && row.auto == auto));
        if let Some(expanded) = self.hub.quick_start.expanded.as_mut() {
            let stage =
                prompt_stage.unwrap_or_else(|| remembered.map_or(Stage::Profiles, |profile| Stage::Modes { profile }));
            expanded.replace_rows(stage, rows, version);
        }
    }

    pub(super) fn apply_quick_start_event(
        &mut self,
        ctx: &Context,
        panel_id: PanelId,
        stage: Stage,
        event: StripEvent,
    ) {
        if stage == Stage::Collapsed {
            if event == StripEvent::Toggle {
                self.open_quick_start_strip(panel_id);
            }
            return;
        }
        if event == StripEvent::OpenPrompts {
            // The list may have changed since the strip opened.
            let prompts = self.build_prompt_rows(panel_id);
            if let Some(expanded) = self
                .hub
                .quick_start
                .expanded
                .as_mut()
                .filter(|expanded| expanded.panel_id == panel_id)
            {
                expanded.set_prompts(prompts);
            }
        }
        let Some(expanded) = self
            .hub
            .quick_start
            .expanded
            .as_mut()
            .filter(|expanded| expanded.panel_id == panel_id)
        else {
            return;
        };
        if let StripEvent::Pick(index) = event
            && expanded.stage == Stage::Profiles
            && expanded.rows.get(index).is_some_and(|row| row.auto && row.dim)
        {
            // Auto with nothing to pick: say so now instead of after the mode page.
            self.hub
                .quick_start
                .show_failure(panel_id, StartFailure::NoAccount, Instant::now());
            return;
        }
        let (next, request, inserted) = match expanded.stage.apply_prompts(event, expanded.prompts.len()) {
            Some((next, insert)) => (next, None, insert),
            None => {
                let (next, request) = expanded.stage.apply(event, expanded.rows.len());
                (next, request, None)
            }
        };
        let started = request.and_then(|StartRequest { profile, mode }| {
            expanded.rows.get(profile).map(|row| (row.name.clone(), row.auto, mode))
        });
        let typed = inserted
            .and_then(|PromptInsert { prompt, send }| expanded.prompts.get(prompt).map(|row| (row.name.clone(), send)));
        expanded.stage = next;
        if next == Stage::Collapsed {
            self.hub.quick_start.expanded = None;
        }
        if let Some((name, send)) = typed {
            self.insert_prompt_named(ctx, panel_id, &name, send);
        }
        if let Some((name, auto, mode)) = started {
            // Auto is resolved now, from the usage as it is at this moment.
            let target = if auto {
                self.current_auto_pick()
                    .map(|pick| pick.name)
                    .ok_or(StartFailure::NoAccount)
            } else {
                Ok(name.clone())
            };
            if let Err(failure) = target.and_then(|target| self.run_claude_quick_start(ctx, panel_id, &target, mode)) {
                tracing::warn!(profile = %name, ?failure, "quick start did not run");
                self.hub.quick_start.show_failure(panel_id, failure, Instant::now());
            }
        }
    }

    /// Types the start command into the panel's shell (bracketed paste when the terminal has it,
    /// Enter as a separate write), through the same board path as `horizon link send`. A panel
    /// that vanished meanwhile is not a failure worth showing.
    fn run_claude_quick_start(
        &mut self,
        ctx: &Context,
        panel_id: PanelId,
        profile_name: &str,
        mode: QuickStartMode,
    ) -> Result<(), StartFailure> {
        let preset = find_profile_preset(&self.presets, profile_name)
            .cloned()
            .ok_or(StartFailure::NoPreset)?;
        let Some(panel) = self.board.panel(panel_id) else {
            return Ok(());
        };
        let is_ssh = panel.kind == PanelKind::Ssh || panel.ssh_connection.is_some();
        // Fresh look: the cached answer may predate a command typed since.
        let exited = panel.child_exited();
        let busy = !exited && panel.child_pid().is_some_and(titlebar::shell_is_busy);
        match Availability::of(panel.kind, is_ssh, busy, exited) {
            Availability::Ready => {}
            Availability::Busy => return Err(StartFailure::Busy),
            Availability::Exited => return Err(StartFailure::Exited),
        }
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let command = quick_start_command(&preset, mode, &home, ShellSyntax::NATIVE).ok_or(StartFailure::Unquotable)?;
        let target = panel.local_id.clone();
        let response = self.board.handle_link_request(LinkRequest::Send {
            target,
            text: command,
            enter: true,
            from: None,
        });
        if let LinkResponse::Err { message, .. } = response {
            tracing::warn!(%message, "quick start could not write to the panel");
            return Err(StartFailure::WriteFailed);
        }
        self.hub.quick_start.busy.forget(panel_id);
        self.board.focus(panel_id);
        ctx.request_repaint();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::claude_auto::AutoChoice;
    use crate::app::test_support::test_app;

    use super::*;

    fn auto_row(target: Option<&str>) -> Row {
        Row::auto(&AutoChoice {
            target: target.map(str::to_owned),
            detail: target.map_or_else(|| "No account available".to_owned(), |name| format!("\u{2192} {name}")),
            low: false,
            tooltip: String::new(),
        })
    }

    fn expand(app: &mut HorizonApp, rows: Vec<Row>) {
        app.hub.quick_start.expanded = Some(Expanded::new(PanelId(1), rows, app.hub.rows_version));
    }

    #[test]
    fn picking_a_dimmed_auto_chip_refuses_at_once_and_keeps_the_profile_page() {
        let mut app = test_app();
        expand(&mut app, vec![auto_row(None)]);

        app.apply_quick_start_event(&Context::default(), PanelId(1), Stage::Profiles, StripEvent::Pick(0));

        let notice = app.hub.quick_start.notice_for(PanelId(1), Instant::now());
        assert_eq!(notice.map(|(failure, _)| failure), Some(StartFailure::NoAccount));
        let expanded = app.hub.quick_start.expanded.as_ref().expect("strip stays open");
        assert_eq!(expanded.stage, Stage::Profiles);
    }

    #[test]
    fn picking_an_available_auto_chip_opens_the_mode_page() {
        let mut app = test_app();
        expand(&mut app, vec![auto_row(Some("conta2"))]);

        app.apply_quick_start_event(&Context::default(), PanelId(1), Stage::Profiles, StripEvent::Pick(0));

        assert_eq!(app.hub.quick_start.notice_for(PanelId(1), Instant::now()), None);
        let expanded = app.hub.quick_start.expanded.as_ref().expect("strip stays open");
        assert_eq!(expanded.stage, Stage::Modes { profile: 0 });
    }
}
