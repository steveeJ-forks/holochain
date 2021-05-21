//! Release command functionality.

use super::*;

use anyhow::bail;
use anyhow::Context;
use cli::ReleaseArgs;
use comrak::{format_commonmark, parse_document, Arena, ComrakOptions};
use enumflags2::{bitflags, BitFlags};
use log::{debug, error, info, trace, warn};
use std::collections::{BTreeSet, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use structopt::StructOpt;

use crate::changelog::{Changelog, WorkspaceCrateReleaseHeading};
pub(crate) use crate_selection::{ReleaseWorkspace, SelectionCriteria};

/// These steps make up the release workflow
#[bitflags]
#[repr(u64)]
#[derive(enum_utils::FromStr, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ReleaseSteps {
    /// branch off of main and merge develop into it
    CreateReleaseBranch,
    /// substeps: get crate selection, bump cargo toml versions, rotate
    /// changelog, aggregate changelog, commit changes, tag
    BumpReleaseVersions,
    PushForPrToMain,
    CreatePrToMain,
    /// verify that the release tag exists on the main branch and is the
    /// second commit on it, directly after the merge commit
    VerifyMainBranch,
    PublishToCratesIo,
    PushReleaseTag,
    BumpPostReleaseVersions,
    PushForDevelopPr,
    CreatePrToDevelop,
}

// todo(backlog): what if at any point during the release process we have to merge a hotfix to main?
// todo: don't forget to adhere to dry-run into all of the following
/// This function handles the release process from start to finish.
/// Eventually this will be idempotent by understanding the state of the repository and
/// derive from it the steps that required to proceed with the release.
///
/// For now it is manual and the release phases need to be given as an instruction.
pub(crate) fn cmd<'a>(
    args: &crate::cli::Args,
    cmd_args: &crate::cli::ReleaseArgs,
) -> CommandResult {
    for step in &cmd_args.steps {
        trace!("Processing step '{:?}'", step);

        // read the workspace after every step in case it was mutated
        let ws = ReleaseWorkspace::try_new_with_criteria(
            args.workspace_path.clone(),
            cmd_args.check_args.to_selection_criteria(),
        )?;

        match step {
            ReleaseSteps::CreateReleaseBranch => create_release_branch(&ws, &cmd_args)?,
            ReleaseSteps::BumpReleaseVersions => bump_release_versions(&ws, cmd_args)?,
            ReleaseSteps::PushForPrToMain => {
                // todo(backlog): push the release branch
                // todo(backlog): create a PR against the main branch
            }
            ReleaseSteps::CreatePrToMain => {}
            ReleaseSteps::VerifyMainBranch => {
                // todo: verify we're on the main branch
                // todo: verify the Pr has been merged
            }
            ReleaseSteps::PublishToCratesIo => {
                // todo: try to publish the crates to crates.io and create a new tag for every published crate
                // todo: for each newly published crate add `github:holochain:core-dev` and `zippy` as an owner on crates.io
            }
            ReleaseSteps::PushReleaseTag => {
                // todo: push all the tags that originated in this workspace release to the upstream
            }
            ReleaseSteps::BumpPostReleaseVersions => {
                // todo: bump versions for every released crate to the next develop version. create a commit for reach crate
                // todo: create a commit that concludes the workspace release?
            }
            ReleaseSteps::PushForDevelopPr => {
                // todo(backlog): push the release branch
            }
            ReleaseSteps::CreatePrToDevelop => {
                // todo(backlog): create a PR against the develop branch
            }
        }
    }

    Ok(())
}

pub(crate) const RELEASE_BRANCH_PREFIX: &str = "release-";

/// Generate a time-derived name for a new release branch.
pub(crate) fn generate_release_branch_name() -> String {
    format!(
        "{}{}",
        RELEASE_BRANCH_PREFIX,
        chrono::Utc::now().format("%Y%m%d.%H%M%S")
    )
}

/// Create a new git release branch.
pub(crate) fn create_release_branch<'a>(
    ws: &'a ReleaseWorkspace<'a>,
    cmd_args: &ReleaseArgs,
) -> Fallible<()> {
    match ws.git_head_branch_name()?.as_str() {
        "develop" => {
            // we're good to continue!
        }
        other => bail!(
            "only support releasing from the 'develop' branch, but found '{}'",
            other
        ),
    };

    let statuses = ws
        .git_repo()
        .statuses(Some(git2::StatusOptions::new().include_untracked(true)))
        .context("querying repository status")?;
    if !statuses.is_empty() {
        bail!(
            "repository is not clean. {} change(s): \n{}",
            statuses.len(),
            statuses
                .iter()
                .map(|statusentry| format!(
                    "{:?}: {}\n",
                    statusentry.status(),
                    statusentry.path().unwrap_or_default()
                ))
                .collect::<String>()
        )
    };

    let release_branch_name = cmd_args
        .release_branch_name
        .to_owned()
        .unwrap_or_else(generate_release_branch_name);

    if cmd_args.dry_run {
        debug!("[dry-run] would create branch '{}'", release_branch_name);
        return Ok(());
    }
    ws.git_checkout_new_branch(&release_branch_name)?;

    ensure_release_branch(&ws)?;

    Ok(())
}

fn bump_release_versions<'a>(
    ws: &'a ReleaseWorkspace<'a>,
    cmd_args: &'a ReleaseArgs,
) -> Fallible<()> {
    let branch_name = match ensure_release_branch(&ws) {
        Ok(branch_name) => branch_name,
        Err(_) if cmd_args.dry_run => generate_release_branch_name(),
        Err(e) => bail!(e),
    };

    // check the workspace and determine the release selection
    let selection = crate::common::selection_check(&cmd_args.check_args, &ws)?;

    let mut changed_crate_changelogs = vec![];

    for crt in &selection {
        let current_version = crt.version();
        let maybe_previous_release_version = crt
            .changelog()
            .ok_or(anyhow::anyhow!(
                "[{}] cannot determine most recent release: missing changelog"
            ))?
            .topmost_release()?
            .map(|change| semver::Version::parse(&change.title()))
            .transpose()?;

        let release_version = if let Some(mut previous_release_version) =
            maybe_previous_release_version.clone()
        {
            if previous_release_version > current_version {
                bail!("previously documented release version '{}' is greater than this release version '{}'", previous_release_version, current_version);
            }

            // todo(backlog): support configurable major/minor/patch/rc? version bumps
            previous_release_version.increment_patch();

            previous_release_version
        } else {
            // release the current version, or bump if the current version is a pre-release
            let mut new_version = current_version.clone();

            if new_version.is_prerelease() {
                // todo(backlog): support configurable major/minor/patch/rc? version bumps
                new_version.increment_patch();
            }

            new_version
        };

        trace!(
            "[{}] previous release version: '{:?}', current version: '{}', release version: '{}' ",
            crt.name(),
            maybe_previous_release_version,
            current_version,
            release_version,
        );

        let greater_release = release_version > current_version;
        if greater_release {
            let cargo_toml_path = crt.root().join("Cargo.toml");
            cargo_next::set_version(&cargo_toml_path, release_version.to_string())?;

            for dependent in crt.dependents_in_workspace()? {
                trace!(
                    "[{}] updating dependency version from dependent {}",
                    crt.name(),
                    dependent.name()
                );

                set_dependency_version(
                    &dependent.root().join("Cargo.toml"),
                    &crt.name(),
                    release_version.to_string().as_str(),
                )?;
            }
        }

        let crate_release_heading_name = format!("{}", release_version);
        // todo: remove or use
        let _crate_tag_name = format!("{}-v{}", crt.name(), crate_release_heading_name);

        if maybe_previous_release_version.is_none() || greater_release {
            trace!(
                "[{}] creating crate release heading '{}'",
                crt.name(),
                crate_release_heading_name
            );

            // create a new release entry in the crate's changelog and move all items from the unreleased heading if there are any
            let changelog = crt
                .changelog()
                .ok_or(anyhow::anyhow!("{} doesn't have changelog", crt.name()))?;

            changelog
                .add_release(crate_release_heading_name.clone())
                .context(format!("adding release to changelog for '{}'", crt.name()))?;

            changed_crate_changelogs.push(WorkspaceCrateReleaseHeading {
                prefix: crt.name(),
                suffix: crate_release_heading_name,
                changelog,
            });
        }

        // todo(spike): create a commit (and tag) for the crate release here?
    }

    // ## for the workspace release:
    let workspace_tag_name = branch_name.clone();
    let workspace_release_name = branch_name
        .clone()
        .strip_prefix(RELEASE_BRANCH_PREFIX)
        .ok_or(anyhow::anyhow!(
            "expected branch name to start with prefix '{}'. got instead: {}",
            RELEASE_BRANCH_PREFIX,
            branch_name,
        ))?
        .to_string();

    let ws_changelog = ws
        .changelog()
        .ok_or(anyhow::anyhow!("workspace has no changelog"))?;

    ws_changelog.add_release(workspace_release_name, &changed_crate_changelogs)?;

    // create a release commit with an overview of which crates are included
    let commit_msg = indoc::formatdoc!(
        r#"
        {}

        the following crates are part of this release:
        {}
        "#,
        workspace_tag_name,
        changed_crate_changelogs
            .iter()
            .map(|wcrh| format!("\n- {}", wcrh.title()))
            .collect::<String>()
    );

    ws.git_add_all_and_commit(&commit_msg, None)?;
    for crate_release_title in changed_crate_changelogs
        .iter()
        .map(WorkspaceCrateReleaseHeading::title)
    {
        ws.git_tag(&crate_release_title, false)?;
    }
    ws.git_tag(&workspace_tag_name, false)?;

    Ok(())
}

/// Ensure we're on a branch that starts with `Self::RELEASE_BRANCH_PREFIX`
pub(crate) fn ensure_release_branch<'a>(ws: &'a ReleaseWorkspace<'a>) -> Fallible<String> {
    let branch_name = ws.git_head_branch_name()?;
    if !branch_name.starts_with(RELEASE_BRANCH_PREFIX) {
        bail!(
            "expected branch name with prefix '{}', got '{}'",
            RELEASE_BRANCH_PREFIX,
            branch_name
        );
    }

    Ok(branch_name)
}

// Adapted from https://github.com/sunng87/cargo-release/blob/f94938c3f20ef20bc8f971d59de75574a0b18931/src/cargo.rs#L122-L154
fn set_dependency_version(manifest_path: &Path, name: &str, version: &str) -> Fallible<()> {
    let temp_manifest_path = manifest_path
        .parent()
        .ok_or(anyhow::anyhow!(
            "couldn't get parent of path {}",
            manifest_path.display()
        ))?
        .join("Cargo.toml.work");

    {
        let manifest = load_from_file(manifest_path)?;
        let mut manifest: toml_edit::Document = manifest.parse()?;
        for key in &["dependencies", "dev-dependencies", "build-dependencies"] {
            if manifest.as_table().contains_key(key)
                && manifest[key]
                    .as_table()
                    .expect("manifest is already verified")
                    .contains_key(name)
            {
                manifest[key][name]["version"] = toml_edit::value(version);
            }
        }

        let mut file_out = std::fs::File::create(&temp_manifest_path)?;
        file_out.write(manifest.to_string_in_original_order().as_bytes())?;
    }
    std::fs::rename(temp_manifest_path, manifest_path)?;

    Ok(())
}

#[cfg(test)]
pub(crate) fn get_dependency_version(manifest_path: &Path, name: &str) -> Fallible<String> {
    let manifest_path = manifest_path
        .parent()
        .ok_or(anyhow::anyhow!(
            "couldn't get parent of path {}",
            manifest_path.display()
        ))?
        .join("Cargo.toml");

    {
        let manifest: toml_edit::Document = load_from_file(&manifest_path)?.parse()?;
        for key in &["dependencies", "dev-dependencies", "build-dependencies"] {
            if manifest.as_table().contains_key(key)
                && manifest[key]
                    .as_table()
                    .expect("manifest is already verified")
                    .contains_key(name)
            {
                return Ok(manifest[key][name]["version"]
                    .as_value()
                    .ok_or(anyhow::anyhow!("expected a value"))?
                    .to_string());
            }
        }
    }

    bail!("version not found")
}

fn load_from_file(path: &Path) -> Fallible<String> {
    let mut file = std::fs::File::open(path)?;
    let mut s = String::new();
    file.read_to_string(&mut s)?;
    Ok(s)
}
