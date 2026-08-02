//! GitHub **releases** repo (public binaries) — separate from private source.
//!
//! Edit `UPDATES_OWNER` once to match your GitHub username or org.

pub const UPDATES_OWNER: &str = "steeb-k";
pub const UPDATES_REPO: &str = "diskoria-binaries";

// Only the (Windows-gated) update flow calls this today; un-gated by the
// port's update phase.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn api_latest_releases_url() -> String {
    format!(
        "https://api.github.com/repos/{UPDATES_OWNER}/{UPDATES_REPO}/releases/latest"
    )
}

/// Browser URL for the releases repo. Kept for a future "view all releases"
/// link in the update UI; not wired up yet.
#[allow(dead_code)]
pub fn releases_repo_page_url() -> String {
    format!("https://github.com/{UPDATES_OWNER}/{UPDATES_REPO}")
}
