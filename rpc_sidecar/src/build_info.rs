const CLIENT_NAME: &str = "CasperSidecar";
const SHORT_GIT_SHA_LENGTH: usize = 8;

#[cfg(not(test))]
pub(crate) fn sidecar_release_version() -> String {
    format_release_version(env!("CARGO_PKG_VERSION"), option_env!("VERGEN_GIT_SHA"))
}

pub(crate) fn web3_client_version() -> String {
    format_client_version(
        env!("CARGO_PKG_VERSION"),
        option_env!("VERGEN_GIT_SHA"),
        env!("VERGEN_CARGO_TARGET_TRIPLE"),
        env!("VERGEN_RUSTC_SEMVER"),
    )
}

fn format_client_version(
    package_version: &str,
    git_sha: Option<&str>,
    target_triple: &str,
    rustc_semver: &str,
) -> String {
    let release_version = format_release_version(package_version, git_sha);
    format!("{CLIENT_NAME}/v{release_version}/{target_triple}/rustc{rustc_semver}")
}

fn format_release_version(package_version: &str, git_sha: Option<&str>) -> String {
    match git_sha.and_then(short_git_sha) {
        Some(git_sha) => format!("{package_version}-{git_sha}"),
        None => package_version.to_string(),
    }
}

fn short_git_sha(git_sha: &str) -> Option<String> {
    let git_sha = git_sha.trim();
    if git_sha.len() < SHORT_GIT_SHA_LENGTH || !git_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }

    Some(git_sha[..SHORT_GIT_SHA_LENGTH].to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_client_version_from_build_metadata() {
        assert_eq!(
            format_client_version(
                "2.0.0",
                Some("F55A64301F1A2821B31A572640C37E48A88321B3"),
                "aarch64-apple-darwin",
                "1.91.0",
            ),
            "CasperSidecar/v2.0.0-f55a6430/aarch64-apple-darwin/rustc1.91.0"
        );
    }

    #[test]
    fn truncates_git_sha_to_eight_characters() {
        assert_eq!(
            format_release_version("2.0.0", Some("0123456789abcdef")),
            "2.0.0-01234567"
        );
    }

    #[test]
    fn omits_git_sha_when_unavailable() {
        assert_eq!(
            format_client_version("2.0.0", None, "x86_64-unknown-linux-gnu", "1.91.0"),
            "CasperSidecar/v2.0.0/x86_64-unknown-linux-gnu/rustc1.91.0"
        );
    }
}
