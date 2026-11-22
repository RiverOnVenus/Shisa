//! The polkit policy decides how often the app-lock password is asked for.
//! Moving either action between `auth_admin` and `auth_admin_keep` changes the
//! prompt cadence and the threat model (docs/architecture.md#threat-model), so
//! it should not ride along unnoticed.

const POLICY: &str = include_str!("../../../data/io.github.RiverOnVenus.Shisa.policy");

fn action(id: &str) -> &'static str {
    let start = POLICY
        .find(&format!(
            "<action id=\"io.github.RiverOnVenus.Shisa.{id}\">"
        ))
        .expect("action is declared");
    let end = POLICY[start..].find("</action>").expect("action is closed");
    &POLICY[start..start + end]
}

#[test]
fn settings_changes_may_reuse_a_recent_authorization() {
    let change = action("change");
    assert!(change.contains("<allow_any>no</allow_any>"));
    assert!(change.contains("<allow_inactive>auth_admin_keep</allow_inactive>"));
    assert!(change.contains("<allow_active>auth_admin_keep</allow_active>"));
}

#[test]
fn launches_ask_for_authentication_each_time() {
    let launch = action("launch");
    assert!(launch.contains("<allow_any>no</allow_any>"));
    assert!(launch.contains("<allow_inactive>auth_admin</allow_inactive>"));
    assert!(launch.contains("<allow_active>auth_admin</allow_active>"));
}
