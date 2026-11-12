use super::protocol::{LinkError, PanelInfo};

/// Resolves a user-typed target: exact id, then exact name, then case-insensitive name.
///
/// # Errors
///
/// `NotFound` when nothing matches, `Ambiguous` (candidates listed) when a name tier
/// matches more than one panel.
pub fn resolve_target<'a>(panels: &'a [PanelInfo], target: &str) -> Result<&'a PanelInfo, LinkError> {
    if target.is_empty() {
        return Err(LinkError::NotFound(String::new()));
    }
    if let Some(panel) = panels.iter().find(|panel| panel.id == target) {
        return Ok(panel);
    }
    let exact: Vec<&PanelInfo> = panels.iter().filter(|panel| panel.name == target).collect();
    if !exact.is_empty() {
        return single(target, &exact);
    }
    let folded: Vec<&PanelInfo> = panels
        .iter()
        .filter(|panel| panel.name.to_lowercase() == target.to_lowercase())
        .collect();
    single(target, &folded)
}

fn single<'a>(target: &str, matches: &[&'a PanelInfo]) -> Result<&'a PanelInfo, LinkError> {
    match matches {
        [] => Err(LinkError::NotFound(target.to_owned())),
        [only] => Ok(only),
        many => Err(LinkError::Ambiguous {
            target: target.to_owned(),
            candidates: many
                .iter()
                .map(|panel| format!("{} ({}, {})", panel.id, panel.name, panel.workspace))
                .collect::<Vec<_>>()
                .join("; "),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::LinkErrorCode;

    fn panel(id: &str, name: &str, workspace: &str) -> PanelInfo {
        PanelInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            workspace: workspace.to_owned(),
            kind: "shell".to_owned(),
            is_agent: false,
        }
    }

    #[test]
    fn resolve_exact_id_beats_name() {
        let panels = [panel("1", "2", "w"), panel("2", "other", "w")];
        assert_eq!(resolve_target(&panels, "2").unwrap().name, "other");
    }

    #[test]
    fn resolve_exact_name_beats_case_insensitive() {
        let panels = [panel("1", "Build", "w"), panel("2", "build", "w")];
        assert_eq!(resolve_target(&panels, "build").unwrap().id, "2");
    }

    #[test]
    fn resolve_case_insensitive_fallback() {
        let panels = [panel("1", "Build", "w"), panel("2", "tests", "w")];
        assert_eq!(resolve_target(&panels, "BUILD").unwrap().id, "1");
    }

    #[test]
    fn resolve_ambiguous_lists_candidates() {
        let panels = [panel("1", "claude", "alpha"), panel("2", "claude", "beta")];
        let err = resolve_target(&panels, "claude").unwrap_err();
        assert_eq!(err.code(), LinkErrorCode::Ambiguous);
        let message = err.to_string();
        assert!(message.contains("1 (claude, alpha)"), "{message}");
        assert!(message.contains("2 (claude, beta)"), "{message}");
        let folded = [panel("1", "Claude", "a"), panel("2", "CLAUDE", "b")];
        let err = resolve_target(&folded, "claude").unwrap_err();
        assert_eq!(err.code(), LinkErrorCode::Ambiguous);
    }

    #[test]
    fn resolve_not_found() {
        let panels = [panel("1", "claude", "w")];
        let err = resolve_target(&panels, "nope").unwrap_err();
        assert_eq!(err.code(), LinkErrorCode::NotFound);
        assert_eq!(resolve_target(&[], "x").unwrap_err().code(), LinkErrorCode::NotFound);
    }

    #[test]
    fn resolve_empty_target_never_matches_unnamed_panel() {
        let panels = [panel("1", "", "w")];
        assert_eq!(resolve_target(&panels, "").unwrap_err().code(), LinkErrorCode::NotFound);
    }
}
