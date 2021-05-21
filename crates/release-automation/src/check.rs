//! Check command functionality.

use super::*;

// use anyhow::bail;
// use anyhow::Context;
// use comrak::{format_commonmark, parse_document, Arena, ComrakOptions};
// use enumflags2::{bitflags, BitFlags};
// use log::{debug, error, info, trace, warn};
// use std::collections::{BTreeSet, HashSet};
// use structopt::StructOpt;

// pub(crate) use crate_selection::{ReleaseWorkspace, SelectionCriteria};

/// Parses the workspace for release candidates and checks for blocking conditions.
pub(crate) fn cmd<'a>(args: &cli::Args, cmd_args: &cli::CheckArgs) -> CommandResult {
    let ws = crate_selection::ReleaseWorkspace::try_new_with_criteria(
        args.workspace_path.clone(),
        cmd_args.to_selection_criteria(),
    )?;

    let release_candidates = common::selection_check(cmd_args, &ws)?;

    println!(
        "{}",
        crate_selection::CrateState::format_crates_states(
            &release_candidates
                .iter()
                .map(|member| (member.name(), member.state()))
                .collect::<Vec<_>>(),
            "The following crates would have been selected for the release process.",
            false,
            true,
            false,
        )
    );

    Ok(())
}
