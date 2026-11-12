//! Saved prompts: named texts the user inserts into a terminal panel from the command palette.
//!
//! They live in `config.yaml` twice: a global `prompts` list and one per workspace template.
//! Both are optional and left out of the file while empty, so configs written before this
//! feature load and re-serialise unchanged.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::WorkspaceConfig;
use crate::error::{Error, Result};

/// Longest prompt name, in characters.
pub const MAX_PROMPT_NAME_CHARS: usize = 40;
/// Longest prompt text, in bytes.
pub const MAX_PROMPT_TEXT_BYTES: usize = 8192;

/// A named text to paste into a terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPrompt {
    pub name: String,
    pub text: String,
}

/// What is wrong with one prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptIssueKind {
    EmptyName,
    NameTooLong,
    NameHasControlChars,
    /// Another prompt of the same list has this name (case-insensitively).
    DuplicateName,
    EmptyText,
    TextTooLong,
    /// NUL, ESC or another control character other than newline and tab.
    TextHasControlChars,
}

impl PromptIssueKind {
    /// Short inline message for the settings editor.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::EmptyName => "Name is required",
            Self::NameTooLong => "Too long",
            Self::NameHasControlChars => "Name can't contain control characters",
            Self::DuplicateName => "Name already used",
            Self::EmptyText => "Text is required",
            Self::TextTooLong => "Too long",
            Self::TextHasControlChars => "Text can't contain control characters",
        }
    }

    /// Whether the problem is with the name field (else with the text field).
    #[must_use]
    pub const fn is_name_issue(self) -> bool {
        matches!(
            self,
            Self::EmptyName | Self::NameTooLong | Self::NameHasControlChars | Self::DuplicateName
        )
    }
}

/// One problem found in a prompt list; `index` points into that list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptIssue {
    pub index: usize,
    pub kind: PromptIssueKind,
}

/// Case-insensitive key two prompt names collide on.
fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Checks `list` and returns every problem, in list order. Newlines and tabs are fine in a
/// text; other control characters (NUL, ESC, ...) are not.
#[must_use]
pub fn validate_prompts(list: &[SavedPrompt]) -> Vec<PromptIssue> {
    let mut issues = Vec::new();
    let mut seen = HashSet::new();
    for (index, prompt) in list.iter().enumerate() {
        let name = prompt.name.trim();
        let mut flag = |kind| issues.push(PromptIssue { index, kind });
        if name.is_empty() {
            flag(PromptIssueKind::EmptyName);
        } else {
            if name.chars().count() > MAX_PROMPT_NAME_CHARS {
                flag(PromptIssueKind::NameTooLong);
            }
            if name.chars().any(char::is_control) {
                flag(PromptIssueKind::NameHasControlChars);
            }
            if !seen.insert(name_key(name)) {
                flag(PromptIssueKind::DuplicateName);
            }
        }
        if prompt.text.trim().is_empty() {
            flag(PromptIssueKind::EmptyText);
        } else {
            if prompt.text.len() > MAX_PROMPT_TEXT_BYTES {
                flag(PromptIssueKind::TextTooLong);
            }
            if prompt
                .text
                .chars()
                .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
            {
                flag(PromptIssueKind::TextHasControlChars);
            }
        }
    }
    issues
}

/// Where an effective prompt comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptScope {
    Global,
    Workspace,
}

impl PromptScope {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }
}

/// A prompt as the user sees it for one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectivePrompt {
    pub name: String,
    pub text: String,
    pub scope: PromptScope,
}

/// The prompts available in a workspace: its own first, then the global ones it does not
/// shadow. A workspace prompt hides the global prompt of the same name (case-insensitively).
#[must_use]
pub fn effective_prompts(global: &[SavedPrompt], workspace: &[SavedPrompt]) -> Vec<EffectivePrompt> {
    let shadowed: HashSet<String> = workspace.iter().map(|prompt| name_key(&prompt.name)).collect();
    let own = workspace.iter().map(|prompt| (prompt, PromptScope::Workspace));
    let inherited = global
        .iter()
        .filter(|prompt| !shadowed.contains(&name_key(&prompt.name)))
        .map(|prompt| (prompt, PromptScope::Global));
    own.chain(inherited)
        .map(|(prompt, scope)| EffectivePrompt {
            name: prompt.name.trim().to_owned(),
            text: prompt.text.clone(),
            scope,
        })
        .collect()
}

/// Validates the global list and every workspace template's list.
///
/// # Errors
///
/// Returns [`Error::Config`] naming the first offending prompt.
pub fn validate_all_prompts(global: &[SavedPrompt], workspaces: &[WorkspaceConfig]) -> Result<()> {
    let describe = |scope: &str, list: &[SavedPrompt]| -> Result<()> {
        match validate_prompts(list).first() {
            None => Ok(()),
            Some(issue) => Err(Error::Config(format!(
                "{scope} prompt #{}: {}",
                issue.index + 1,
                issue.kind.message()
            ))),
        }
    };
    describe("prompts:", global)?;
    for workspace in workspaces {
        describe(&format!("workspace '{}':", workspace.name), &workspace.prompts)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(name: &str, text: &str) -> SavedPrompt {
        SavedPrompt {
            name: name.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_plain_list_has_no_issues() {
        let list = [prompt("review", "Review this diff.\nBe terse.\n\tIndented")];

        assert!(validate_prompts(&list).is_empty());
    }

    #[test]
    fn empty_name_and_text_are_reported_per_row() {
        let list = [prompt("  ", "x"), prompt("ok", "")];

        let kinds: Vec<_> = validate_prompts(&list).iter().map(|i| (i.index, i.kind)).collect();

        assert_eq!(
            kinds,
            [(0, PromptIssueKind::EmptyName), (1, PromptIssueKind::EmptyText)]
        );
    }

    #[test]
    fn duplicate_names_ignore_case_and_flag_the_later_row() {
        let list = [prompt("Review", "a"), prompt("review ", "b"), prompt("other", "c")];

        let issues = validate_prompts(&list);

        assert_eq!(
            issues,
            [PromptIssue {
                index: 1,
                kind: PromptIssueKind::DuplicateName
            }]
        );
    }

    #[test]
    fn name_length_counts_characters_not_bytes() {
        let ok = "\u{e9}".repeat(MAX_PROMPT_NAME_CHARS);
        let long = "a".repeat(MAX_PROMPT_NAME_CHARS + 1);

        assert!(validate_prompts(&[prompt(&ok, "x")]).is_empty());
        assert_eq!(
            validate_prompts(&[prompt(&long, "x")])[0].kind,
            PromptIssueKind::NameTooLong
        );
    }

    #[test]
    fn text_over_the_byte_cap_is_too_long() {
        let at_cap = "a".repeat(MAX_PROMPT_TEXT_BYTES);
        let over = "a".repeat(MAX_PROMPT_TEXT_BYTES + 1);

        assert!(validate_prompts(&[prompt("n", &at_cap)]).is_empty());
        assert_eq!(
            validate_prompts(&[prompt("n", &over)])[0].kind,
            PromptIssueKind::TextTooLong
        );
    }

    #[test]
    fn control_characters_are_rejected_except_newline_and_tab() {
        for bad in ["a\u{0}b", "a\u{1b}[31mb", "a\u{7}b", "a\rb"] {
            let issues = validate_prompts(&[prompt("n", bad)]);
            assert_eq!(issues[0].kind, PromptIssueKind::TextHasControlChars, "{bad:?}");
        }
        assert_eq!(
            validate_prompts(&[prompt("a\nb", "x")])[0].kind,
            PromptIssueKind::NameHasControlChars
        );
    }

    #[test]
    fn workspace_prompts_come_first_and_shadow_the_global_one() {
        let global = [prompt("review", "global"), prompt("tests", "write tests")];
        let workspace = [prompt("Review", "local"), prompt("deploy", "ship it")];

        let list = effective_prompts(&global, &workspace);

        let view: Vec<_> = list
            .iter()
            .map(|p| (p.name.as_str(), p.text.as_str(), p.scope))
            .collect();
        assert_eq!(
            view,
            [
                ("Review", "local", PromptScope::Workspace),
                ("deploy", "ship it", PromptScope::Workspace),
                ("tests", "write tests", PromptScope::Global),
            ]
        );
    }

    #[test]
    fn without_workspace_prompts_the_global_list_is_kept_in_order() {
        let global = [prompt("b", "1"), prompt("a", "2")];

        let list = effective_prompts(&global, &[]);

        assert_eq!(list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert!(list.iter().all(|p| p.scope == PromptScope::Global));
    }
}
