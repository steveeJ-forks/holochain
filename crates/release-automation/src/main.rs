#![allow(unused_imports)]
#![allow(dead_code)]

use anyhow::bail;
use anyhow::Context;
use comrak::{format_commonmark, parse_document, Arena, ComrakOptions};
use enumflags2::{bitflags, BitFlags};
use log::{debug, error, info, trace, warn};
use std::collections::{BTreeSet, HashSet};
use structopt::StructOpt;

pub(crate) mod changelog;
pub(crate) mod check;
pub(crate) mod common;
pub(crate) mod crate_selection;
pub(crate) mod release;

#[cfg(test)]
pub(crate) mod tests;

use crate_selection::{aliases::CargoDepKind, CrateState, CrateStateFlags};
use release::ReleaseSteps;

type Fallible<T> = anyhow::Result<T>;
type CommandResult = Fallible<()>;

pub(crate) mod cli {
    use super::*;
    use crate_selection::SelectionCriteria;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    #[derive(Debug, StructOpt)]
    #[structopt(name = "release-automation")]
    pub(crate) struct Args {
        #[structopt(long)]
        pub(crate) workspace_path: PathBuf,

        #[structopt(subcommand)]
        pub(crate) cmd: Commands,

        #[structopt(long, default_value = "warn")]
        pub(crate) log_level: log::Level,
    }

    #[derive(Debug, StructOpt)]
    #[structopt(name = "ra")]
    pub(crate) enum Commands {
        Changelog(ChangelogArgs),
        Release(ReleaseArgs),
        Check(CheckArgs),
    }

    #[derive(Debug, StructOpt)]
    pub(crate) struct ChangelogAggregateArgs {
        /// Allows a specified subset of crates that will haveh their changelog aggregated in the workspace changelog.
        /// This string will be used as a regex to filter the package names.
        /// By default, all crates will be considered.
        #[structopt(long, default_value = ".*")]
        pub(crate) selection_filter: fancy_regex::Regex,

        /// Output path, relative to the workspace root.
        #[structopt(long, default_value = "CHANGELOG.md")]
        pub(crate) output_path: PathBuf,
    }

    #[derive(Debug, StructOpt)]
    pub(crate) enum ChangelogCommands {
        Aggregate(ChangelogAggregateArgs),
    }

    #[derive(StructOpt, Debug)]
    pub(crate) struct ChangelogArgs {
        #[structopt(subcommand)]
        pub(crate) command: ChangelogCommands,
    }

    /// Determine whether there are any release blockers by analyzing the state of the workspace.
    #[derive(StructOpt, Debug)]
    pub(crate) struct CheckArgs {
        /// All existing versions must match these requirements.
        /// Can be passed more than once to specify multiple.
        /// See https://docs.rs/semver/0.11.0/semver/?search=#requirements
        #[structopt(long)]
        pub(crate) enforced_version_reqs: Vec<semver::VersionReq>,

        /// None of the existing versions are allowed to match these requirements.
        /// Can be passed more than once to specify multiple.
        /// See https://docs.rs/semver/0.11.0/semver/?search=#requirements
        #[structopt(long)]
        pub(crate) disallowed_version_reqs: Vec<semver::VersionReq>,

        /// Allows a specified subset of crates to be released by regex matches on the crates' package name.
        /// This string will be used as a regex to filter the package names.
        /// By default, all crates will be considered release candidates.
        #[structopt(long, default_value = ".*")]
        pub(crate) selection_filter: fancy_regex::Regex,

        /// Allow these blocking states for dependency crates.
        /// Comma separated.
        /// Valid values are: MissingReadme, UnreleasableViaChangelogFrontmatter, DisallowedVersionReqViolated, EnforcedVersionReqViolated
        #[structopt(long, default_value = "", parse(try_from_str = parse_cratestateflags))]
        pub(crate) allowed_dependency_blockers: BitFlags<CrateStateFlags>,

        /// Allow these blocking states for crates via the packages filter.
        /// Comma separated.
        /// Valid values are: MissingReadme, UnreleasableViaChangelogFrontmatter, DisallowedVersionReqViolated, EnforcedVersionReqViolated
        #[structopt(long, default_value = "", parse(try_from_str = parse_cratestateflags))]
        pub(crate) allowed_selection_blockers: BitFlags<CrateStateFlags>,

        /// These dependency types will be ignored.
        /// Comma separated.
        /// Valid values are: normal, build, development
        #[structopt(long, default_value="", parse(try_from_str = parse_depkind))]
        pub(crate) exclude_dep_kinds: HashSet<CargoDepKind>,

        /// Exclude optional dependencies.
        #[structopt(long)]
        pub(crate) exclude_optional_deps: bool,
    }

    fn parse_depkind(input: &str) -> Fallible<HashSet<CargoDepKind>> {
        let mut set = HashSet::new();

        for word in input.split(",") {
            set.insert(match word.to_lowercase().as_str() {
                "" => continue,
                "normal" => CargoDepKind::Normal,
                "development" => CargoDepKind::Development,
                "build" => CargoDepKind::Build,

                invalid => bail!("invalid dependency kind: {}", invalid),
            });
        }

        Ok(set)
    }

    fn parse_cratestateflags(input: &str) -> Fallible<BitFlags<CrateStateFlags>> {
        use std::str::FromStr;

        input
            .split(",")
            .filter(|s| !s.is_empty())
            .map(|csf| {
                CrateStateFlags::from_str(csf)
                    .map_err(|_| anyhow::anyhow!("could not parse '{}' as CrateStateFlags", input))
            })
            .try_fold(
                Default::default(),
                |mut acc, elem| -> Fallible<BitFlags<_>> {
                    acc.insert(elem?);
                    Ok(acc)
                },
            )
    }

    impl CheckArgs {
        /// Boilerplate to instantiate `SelectionCriteria` from `CheckArgs`
        pub(crate) fn to_selection_criteria(&self) -> SelectionCriteria {
            SelectionCriteria {
                selection_filter: self.selection_filter.clone(),
                disallowed_version_reqs: self.disallowed_version_reqs.clone(),
                enforced_version_reqs: self.enforced_version_reqs.clone(),
                allowed_dependency_blockers: self.allowed_dependency_blockers.clone(),
                allowed_selection_blockers: self.allowed_selection_blockers.clone(),
                exclude_dep_kinds: self.exclude_dep_kinds.clone(),
                exclude_optional_deps: self.exclude_optional_deps,
            }
        }
    }

    /// Initiate a release process with the given arguments.
    ///
    /// See https://docs.rs/semver/0.11.0/semver/?search=#requirements for details on the requirements arguments.
    #[derive(StructOpt, Debug)]
    pub(crate) struct ReleaseArgs {
        #[structopt(flatten)]
        pub(crate) check_args: CheckArgs,

        #[structopt(long)]
        pub(crate) dry_run: bool,

        /// Will be inferred from the current name if not given.
        #[structopt(long)]
        pub(crate) release_branch_name: Option<String>,

        /// The release steps to perform.
        /// These will be reordered to their defined ordering.
        ///
        /// See `ReleaseSteps` for the list of steps.
        #[structopt(long, default_value="", parse(try_from_str = parse_releasesteps))]
        pub(crate) steps: BTreeSet<ReleaseSteps>,
    }

    /// Parses an input string to an ordered set of release steps.
    pub(crate) fn parse_releasesteps(input: &str) -> Fallible<BTreeSet<ReleaseSteps>> {
        use std::str::FromStr;

        input
            .split(",")
            .filter(|s| !s.is_empty())
            .map(|csf| {
                ReleaseSteps::from_str(csf)
                    .map_err(|_| anyhow::anyhow!("could not parse '{}' as ReleaseSteps", input))
            })
            .try_fold(
                Default::default(),
                |mut acc, elem| -> Fallible<BTreeSet<_>> {
                    acc.insert(elem?);
                    Ok(acc)
                },
            )
    }
}

fn main() -> CommandResult {
    let args = cli::Args::from_args();

    env_logger::builder()
        .filter_level(args.log_level.to_level_filter())
        .format_timestamp(None)
        .init();

    debug!("args: {:#?}", args);

    match &args.cmd {
        cli::Commands::Changelog(cmd_args) => crate::changelog::cmd(&args, cmd_args),
        cli::Commands::Check(cmd_args) => crate::check::cmd(&args, cmd_args),
        cli::Commands::Release(cmd_args) => crate::release::cmd(&args, cmd_args),
    }
}
