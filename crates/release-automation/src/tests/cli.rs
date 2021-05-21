use crate::changelog::sanitize;
use crate::changelog::{ChangelogT, CrateChangelog, WorkspaceChangelog};
use crate::crate_selection::ReleaseWorkspace;
use crate::tests::workspace_mocker::{
    example_workspace_1, example_workspace_1_aggregated_changelog,
};
use anyhow::Context;
use predicates::prelude::*;

#[test]
fn release_createreleasebranch() {
    let workspace_mocker = example_workspace_1().unwrap();
    let workspace = ReleaseWorkspace::try_new(workspace_mocker.root()).unwrap();
    workspace.git_checkout_new_branch("develop").unwrap();
    let mut cmd = assert_cmd::Command::cargo_bin("release-automation").unwrap();
    let cmd = cmd.args(&[
        &format!("--workspace-path={}", workspace.root().display()),
        "release",
        "--steps=CreateReleaseBranch",
    ]);
    cmd.assert().success();

    crate::release::ensure_release_branch(&workspace).unwrap();
}

#[test]
fn release_createreleasebranch_fails_on_dirty_repo() {
    let workspace_mocker = example_workspace_1().unwrap();
    workspace_mocker.add_or_replace_file(
        "crates/crate_a/README",
        indoc::indoc! {r#"
            # Example

            Some changes
            More changes
            "#,
        },
    );
    let workspace = ReleaseWorkspace::try_new(workspace_mocker.root()).unwrap();
    workspace.git_checkout_new_branch("develop").unwrap();

    let mut cmd = assert_cmd::Command::cargo_bin("release-automation").unwrap();
    let cmd = cmd.args(&[
        &format!("--workspace-path={}", workspace.root().display()),
        "--log-level=debug",
        "release",
        "--steps=CreateReleaseBranch",
    ]);

    cmd.assert()
        .stderr(predicate::str::contains("repository is not clean"))
        .failure();
}

macro_rules! assert_cmd_success {
    ($cmd:expr) => {{
        let output = $cmd.output().unwrap();

        let (stderr, stdout) = (
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout),
        );

        if !output.status.success() {
            panic!(
                "code: {:?}\nstderr:\n'{}'\n---\nstdout:\n'{}'\n---\n",
                output.status.code(),
                stderr,
                stdout,
            );
        };

        (String::from(stderr), String::from(stdout))
    }};
}

// todo(backlog): ensure all of these conditions have unit tests?
#[test]
fn bump_versions_on_selection() {
    let workspace_mocker = example_workspace_1().unwrap();
    let workspace = ReleaseWorkspace::try_new(workspace_mocker.root()).unwrap();
    workspace.git_checkout_new_branch("develop").unwrap();

    let mut cmd = assert_cmd::Command::cargo_bin("release-automation").unwrap();
    let cmd = cmd.args(&[
        &format!("--workspace-path={}", workspace.root().display()),
        "--log-level=trace",
        "release",
        "--disallowed-version-reqs=>=0.1",
        "--allowed-selection-blockers=UnreleasableViaChangelogFrontmatter,DisallowedVersionReqViolated",
        "--steps=CreateReleaseBranch,BumpReleaseVersions",
    ]);

    let output = assert_cmd_success!(cmd);

    // set expectations
    let expected_crates = vec!["crate_b", "crate_a", "crate_e"];
    let expected_release_versions = vec!["0.0.1", "0.0.2", "0.0.1"];

    // check manifests for new release headings
    assert_eq!(
        expected_release_versions,
        expected_crates
            .clone()
            .into_iter()
            .map(|name| {
                let cargo_toml_path = workspace
                    .root()
                    .join("crates")
                    .join(name)
                    .join("Cargo.toml");
                cargo_next::get_version(&cargo_toml_path)
                    .context(format!(
                        "trying to parse version in Cargo.toml at {:?}",
                        cargo_toml_path
                    ))
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>()
    );

    // ensure dependents were updated
    // todo: ensure *all* dependents were updated
    assert_eq!(
        "0.0.1",
        crate::release::get_dependency_version(
            &workspace
                .root()
                .join("crates")
                .join("crate_a")
                .join("Cargo.toml"),
            "crate_b"
        )
        .unwrap()
        .replace("\"", "")
        .replace("\\", "")
        .replace(" ", ""),
    );

    // check changelogs for new release headings
    assert_eq!(
        expected_release_versions,
        expected_crates
            .iter()
            .map(|crt| {
                let path = workspace
                    .root()
                    .join("crates")
                    .join(crt)
                    .join("CHANGELOG.md");
                ChangelogT::<CrateChangelog>::at_path(&path)
                    .topmost_release()
                    .context(format!("querying topmost_release on changelog for {}", crt))
                    .unwrap()
                    .ok_or(anyhow::anyhow!(
                        "changelog for {} doesn't have any release. file content: \n{}",
                        crt,
                        std::fs::read_to_string(&path).unwrap()
                    ))
                    .map(|change| change.title().to_string())
                    .unwrap()
            })
            .collect::<Vec<String>>()
    );

    let workspace_changelog =
        ChangelogT::<WorkspaceChangelog>::at_path(&workspace.root().join("CHANGELOG.md"));
    let topmost_workspace_release = workspace_changelog
        .topmost_release()
        .unwrap()
        .map(|change| change.title().to_string())
        .unwrap();

    // todo: ensure the returned title doesn't contain any brackets?
    assert!(
        // FIXME: make this compatible with years beyond 2099
        topmost_workspace_release.starts_with("[20") || topmost_workspace_release.starts_with("20"),
        "{}",
        topmost_workspace_release
    );
    assert_ne!("[20210304.120604]", topmost_workspace_release);

    {
        // check for release heading contents in the workspace changelog

        let expected = sanitize(indoc::formatdoc!(
            r#"
        # Changelog

        This file conveniently consolidates all of the crates individual CHANGELOG.md files and groups them by timestamps at which crates were released. The file is updated every time one or more crates are released.

        The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/). This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

        # \[Unreleased\]

        The text beneath this heading will be retained which allows adding overarching release notes.

        ## Something outdated maybe

        This will be removed by aggregation.

        ## [crate_c](crates/crate_c/CHANGELOG.md#unreleased)
        Awesome changes!

        ### Breaking
        Breaking changes, be careful.

        ## [crate_f](crates/crate_f/CHANGELOG.md#unreleased)
        This will be released in the future.

        # {}

        ## [crate_b-0.0.1](crates/crate_b/CHANGELOG.md#0.0.1)

        ### Changed

        - `Signature` is a 64 byte ‘secure primitive’

        ## [crate_a-0.0.2](crates/crate_a/CHANGELOG.md#0.0.2)

        ### Added

        - `InstallAppBundle`
        - `DnaSource`

        ### Removed

        - BREAKING:  `InstallAppDnaPayload`
        - BREAKING: `DnaSource(Path)`

        ## [crate_e-0.0.1](crates/crate_e/CHANGELOG.md#0.0.1)

        Awesome changes\!

        # \[20210304.120604\]

        This will include the hdk-0.0.100 release.

        ## [hdk-0.0.100](crates/hdk/CHANGELOG.md#0.0.100)

        ### Changed

        - hdk: fixup the autogenerated hdk documentation.
        "#,
            topmost_workspace_release
        ));

        let result = sanitize(std::fs::read_to_string(workspace_changelog.path()).unwrap());
        assert_eq!(
            result,
            expected,
            "{}",
            prettydiff::text::diff_lines(&result, &expected).format()
        );
    }

    // todo: ensure the git commits for the crate releases were created?
    // todo: ensure the git tags for the crate releases were created?

    // ensure the git commit for the whole release was created
    let commit_msg = {
        let commit = workspace
            .git_repo()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap();

        commit.message().unwrap().to_string()
    };

    assert_eq!(
        indoc::formatdoc!(
            r#"
        release-{}

        the following crates are part of this release:

        - crate_b-0.0.1
        - crate_a-0.0.2
        - crate_e-0.0.1
        "#,
            topmost_workspace_release
        ),
        commit_msg
    );

    for expected_tag in &[
        &format!("release-{}", topmost_workspace_release),
        "crate_b-0.0.1",
        "crate_a-0.0.2",
        "crate_e-0.0.1",
    ] {
        crate::crate_selection::git_lookup_tag(workspace.git_repo(), &expected_tag)
            .expect(&format!("git tag '{}' not found", &expected_tag));
        crate::crate_selection::git_lookup_tag(workspace.git_repo(), &expected_tag)
            .expect(&format!("git tag '{}' not found", &expected_tag));
    }

    if matches!(option_env!("FAIL_CLI_RELEASE_TEST"), Some(_)) {
        println!("stderr:\n'{}'\n---\nstdout:\n'{}'\n---", output.0, output.1,);

        panic!("workspace root: {:?}", workspace.root());
    }
}

#[test]
fn changelog_aggregation() {
    let workspace_mocker = example_workspace_1().unwrap();

    let workspace = ReleaseWorkspace::try_new(workspace_mocker.root()).unwrap();

    let mut cmd = assert_cmd::Command::cargo_bin("release-automation").unwrap();
    let cmd = cmd.args(&[
        &format!("--workspace-path={}", workspace.root().display()),
        "--log-level=trace",
        "changelog",
        "aggregate",
    ]);

    let _output = assert_cmd_success!(cmd);

    let workspace_changelog =
        ChangelogT::<WorkspaceChangelog>::at_path(&workspace.root().join("CHANGELOG.md"));
    let result = sanitize(std::fs::read_to_string(workspace_changelog.path()).unwrap());

    let expected = example_workspace_1_aggregated_changelog();
    assert_eq!(
        result,
        expected,
        "{}",
        prettydiff::text::diff_lines(&result, &expected).format()
    );
}
