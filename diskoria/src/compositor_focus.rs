//! Best-effort "raise my own window" for Wayland compositors.
//!
//! A Wayland client cannot focus itself: activation needs a token from the
//! surface the user clicked, and a StatusNotifierItem host has no way to pass
//! one (KI-35). The compositors people actually run tiling setups on do,
//! however, expose an IPC that can focus a window by pid — which is exactly
//! what a tray click wants. This asks the compositor politely and reports
//! whether it worked; everything degrades to "no" and the caller falls back to
//! an attention request.
//!
//! Elevated runs shell out as the invoking user (`elevation::
//! command_as_session_user`), since the IPC sockets are the user's.

#![cfg(target_os = "linux")]

use std::process::Stdio;

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = crate::elevation::command_as_session_user(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn ok(program: &str, args: &[&str]) -> bool {
    run(program, args).is_some()
}

/// niri: find our window by pid through the JSON IPC, then focus it by id.
fn niri_focus(pid: u32) -> bool {
    if std::env::var_os("NIRI_SOCKET").is_none() {
        return false;
    }
    let Some(json) = run("niri", &["msg", "--json", "windows"]) else {
        return false;
    };
    let Some(id) = window_id_for_pid(&json, pid) else {
        return false;
    };
    ok("niri", &["msg", "action", "focus-window", "--id", &id.to_string()])
}

/// Pick the window belonging to `pid` out of niri's `windows` JSON.
/// Hand-parsed rather than pulling in a JSON dependency for two fields: the
/// objects are flat, so the id preceding the matching pid is unambiguous.
pub(crate) fn window_id_for_pid(json: &str, pid: u32) -> Option<u64> {
    let needle = format!("\"pid\":{pid}");
    let compact: String = json.chars().filter(|c| !c.is_whitespace()).collect();
    // Every candidate, because `"pid":9876` is also a prefix of `"pid":98765`
    // — the match only counts when the number ends there.
    let mut from = 0usize;
    while let Some(rel) = compact[from..].find(&needle) {
        let at = from + rel;
        from = at + needle.len();
        let ends_here = compact[from..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_digit());
        if !ends_here {
            continue;
        }
        // Objects are `{"id":N,...,"pid":M,...}` — take the id of the object
        // the match sits in.
        let Some(obj_start) = compact[..at].rfind('{') else {
            continue;
        };
        let Some(id_rel) = compact[obj_start..at].find("\"id\":") else {
            continue;
        };
        let after_id = obj_start + id_rel + 5;
        let digits: String = compact[after_id..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(id) = digits.parse() {
            return Some(id);
        }
    }
    None
}

fn sway_focus(pid: u32) -> bool {
    std::env::var_os("SWAYSOCK").is_some()
        && ok("swaymsg", &[&format!("[pid={pid}]"), "focus"])
}

fn hyprland_focus(pid: u32) -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        && ok("hyprctl", &["dispatch", "focuswindow", &format!("pid:{pid}")])
}

/// Ask the running compositor to focus this process's window.
/// `false` means "not supported here" — the caller should fall back.
pub fn focus_own_window() -> bool {
    let pid = std::process::id();
    let focused = niri_focus(pid) || sway_focus(pid) || hyprland_focus(pid);
    if focused {
        log::debug!(target: "diskoria", "compositor IPC focused our window (pid {pid})");
    }
    focused
}

#[cfg(test)]
mod tests {
    use super::window_id_for_pid;

    const SAMPLE: &str = r#"[
        {"id":444,"title":"Firefox","app_id":"firefox","pid":1234,"is_focused":false},
        {"id":506,"title":"Diskoria","app_id":"diskoria","pid":9876,"is_focused":true}
    ]"#;

    #[test]
    fn finds_the_window_owned_by_a_pid() {
        assert_eq!(window_id_for_pid(SAMPLE, 9876), Some(506));
        assert_eq!(window_id_for_pid(SAMPLE, 1234), Some(444));
    }

    #[test]
    fn unknown_pid_and_junk_yield_nothing() {
        assert_eq!(window_id_for_pid(SAMPLE, 5), None);
        assert_eq!(window_id_for_pid("not json", 9876), None);
        // A pid that only appears as a prefix of another must not match.
        assert_eq!(window_id_for_pid(r#"[{"id":7,"pid":98765}]"#, 9876), None);
    }
}
