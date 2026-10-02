use std::collections::HashMap;

use launcher::{
    run_sync,
    types::{FeatureRule, Os, OsRule, Rule, RuleAction},
};

fn main() {
    let state = Rule {
        action: RuleAction::Allow,
        features: Some(FeatureRule {
            has_custom_resolution: Some(false),
            has_quick_plays_support: Some(false),
            is_demo_user: Some(false),
            is_quick_play_multiplayer: Some(false),
            is_quick_play_realms: Some(false),
            is_quick_play_singleplayer: Some(false),
        }),
        os: Some(OsRule {
            arch: Some("x86_64".into()),
            name: Some(Os::Linux),
            version: None,
        }),
    };
    pub const MC_VERSION: &str = "26.3";
    pub const NF_VERSION: &str = "26.3.0.41-beta";

    let output = run_sync(state, MC_VERSION.into(), NF_VERSION.into(), HashMap::new())
        .unwrap()
        .output()
        .unwrap();
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        panic!("{err}");
    }
}
