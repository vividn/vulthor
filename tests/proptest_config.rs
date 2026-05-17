//! Property-based fuzz tests for the TOML config parser (vu-wb0).
//!
//! `Config` is deserialized from user-supplied `vulthor.toml` via
//! `toml::from_str`. Malformed input must surface as an `Err`, never
//! a panic. Well-formed but minimal input must parse to a valid `Config`.

use proptest::prelude::*;
use vulthor::config::Config;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        .. ProptestConfig::default()
    })]

    /// Random byte-string TOML must round-trip to `Result<Config, _>`
    /// without ever panicking. We do not assert Err — some inputs may
    /// happen to deserialize into the all-default `Config` — only that
    /// the call returns rather than aborting the process.
    #[test]
    fn malformed_toml_returns_error_not_panic(s in ".{0,2048}") {
        let _ = toml::from_str::<Config>(&s);
    }

    /// TOML that is syntactically valid but semantically arbitrary (random
    /// keys/values at the top level). The parser must classify these as
    /// Err (unknown fields aren't allowed by serde's default behaviour
    /// here) or Ok with the defaults filled in — either way, no panic.
    #[test]
    fn random_keys_do_not_panic(
        pairs in proptest::collection::vec(
            ("[a-z_]{1,16}", "[a-zA-Z0-9 ._@/-]{0,32}"),
            0..16,
        ),
    ) {
        let mut s = String::new();
        for (k, v) in &pairs {
            s.push_str(&format!("{k} = \"{v}\"\n"));
        }
        let _ = toml::from_str::<Config>(&s);
    }
}

/// A skeleton `[web]` block with generated bind/port values. Validates
/// that the parser tolerates a wide range of port / bind string shapes
/// (validation happens later, in `Config::load_from_file`, not in pure
/// deserialization — so this should always succeed).
fn web_block() -> impl Strategy<Value = String> {
    (any::<u16>(), "[a-zA-Z0-9.:_-]{0,32}")
        .prop_map(|(port, bind)| format!("[web]\nport = {port}\nbind = \"{bind}\"\n"))
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        .. ProptestConfig::default()
    })]

    /// A minimal valid config (only the `maildir_path` key + an optional
    /// `[web]` block) must always deserialize successfully.
    #[test]
    fn minimal_valid_config_parses(
        path in "[a-zA-Z0-9./_-]{1,128}",
        include_web in any::<bool>(),
        web in web_block(),
    ) {
        let mut s = format!("maildir_path = \"{path}\"\n");
        if include_web {
            s.push_str(&web);
        }
        let parsed = toml::from_str::<Config>(&s);
        prop_assert!(parsed.is_ok(), "expected Ok, got: {parsed:?}");
    }
}
