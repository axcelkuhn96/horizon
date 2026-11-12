//! What the strip does besides drawing: availability of the button, building and refreshing the
//! profile rows, applying a [`StripEvent`] and typing the start command into the panel's PTY.

use std::time::Instant;

use egui::{Color32, Context};
use horizon_core::link::{LinkRequest, LinkResponse};
use horizon_core::{PanelId, PanelKind};

use crate::app::HorizonApp;
use crate::app::claude_auto::MIN_PROFILES_FOR_AUTO;
use crate::app::claude_launcher::{RowUsage, claude_profile_names, find_profile_preset, is_profile_preset};
use crate::app::hub_ui;

use super::command::{ShellSyntax, quick_start_command};
use super::state::{Expanded, Row};
use super::titlebar;
use super::{Availability, QuickStartMode, Stage, StartFailure, StartRequest, StripEvent, quick_start_visible};

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

    fn open_quick_start_strip(&mut self, panel_id: PanelId) {
        let names = claude_profile_names(&self.presets);
        if names.is_empty() {
            return;
        }
        let rows = self.build_quick_rows(names);
        self.hub.quick_start.expanded = Some(Expanded::new(panel_id, rows, self.hub.rows_version));
    }

    /// Rebuilds the open strip's rows when the profile list or the usage data moved.
    pub(super) fn refresh_quick_start_rows(&mut self, panel_id: PanelId) {
        let version = self.hub.rows_version;
        let current = match self
            .hub
            .quick_start
            .expanded
            .as_ref()
            .filter(|expanded| expanded.panel_id == panel_id && expanded.rows_version != version)
        {
            None => return,
            Some(expanded) => match expanded.stage {
                Stage::Modes { profile } => expanded.rows.get(profile).map(|row| (row.name.clone(), row.auto)),
                Stage::Collapsed | Stage::Profiles => None,
            },
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
            let stage = remembered.map_or(Stage::Profiles, |profile| Stage::Modes { profile });
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
            && expanded.rows.get(index).is_some_and(|row| row.auto && row.dim)
        {
            // Auto with nothing to pick: say so now instead of after the mode page.
            self.hub
                .quick_start
                .show_failure(panel_id, StartFailure::NoAccount, Instant::now());
            return;
        }
        let (next, request) = expanded.stage.apply(event, expanded.rows.len());
        let started = request.and_then(|StartRequest { profile, mode }| {
            expanded.rows.get(profile).map(|row| (row.name.clone(), row.auto, mode))
        });
        expanded.stage = next;
        if next == Stage::Collapsed {
            self.hub.quick_start.expanded = None;
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
