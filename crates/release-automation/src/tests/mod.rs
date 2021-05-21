pub(crate) mod cli;
pub(crate) mod workspace_mocker;

use crate::release::ReleaseSteps;

#[test]
fn release_steps_are_ordered() {
    let input = "BumpReleaseVersions,VerifyMainBranch,PushForPrToMain,CreatePrToMain,PublishToCratesIo,PushReleaseTag,BumpPostReleaseVersions,PushForDevelopPr,CreatePrToDevelop,CreateReleaseBranch";

    let parsed = super::cli::parse_releasesteps(input)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();

    assert_eq!(parsed.get(0), Some(&ReleaseSteps::CreateReleaseBranch));

    assert_eq!(parsed.get(1), Some(&ReleaseSteps::BumpReleaseVersions));

    assert_eq!(parsed.last(), Some(&ReleaseSteps::CreatePrToDevelop));
}
