// Phase 5.a — AI classifier interface stub.
//
// VISION.md §AI Classifier promises the classifier interface (trait,
// suggestion chip, `;` accept key) ships in v1 disabled-by-default so
// users can opt in later without code changes. This module owns the
// trait, the [`Suggestion`] value type, and a [`NoopClassifier`] that
// always returns `None`. Phase 6 will plug in the real embeddings
// backend behind `[ai].backend = "embeddings"`; until then
// [`build_classifier`] always hands back the no-op.

use std::sync::Arc;

use crate::config::AiConfig;
use crate::email::Email;
use crate::keymap::Action;

/// A single classifier suggestion for one email. `action` is one of the
/// keymap intents the chip / `;` accept key resolves into; `confidence`
/// is compared against `[ai].threshold` at render time.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// Intent to surface — Archive, Star, Delete, Reply, ReplyAll, or
    /// Forward today. Other variants are accepted by the type but the
    /// chip / accept-key path ignores them.
    pub action: Action,
    /// Model-reported confidence in `[0.0, 1.0]`. Compared against
    /// `[ai].threshold` (default 0.6) at the suggestion site.
    pub confidence: f32,
}

/// Classify an email into an optional suggested [`Action`]. Trait-object
/// safe (`Send + Sync`) so AppRoot can share one instance across the
/// dispatch thread and the render thread via `Arc<dyn Classifier>`.
pub trait Classifier: Send + Sync {
    /// Return a suggestion for `email`, or `None` to abstain. Cheap;
    /// called once per rendered row.
    fn suggest(&self, email: &Email) -> Option<Suggestion>;
}

/// Disabled-by-default classifier. Always returns `None` so the chip
/// never renders and the `;` accept key is a no-op. Used whenever
/// `[ai].enabled = false` (the default) or the real backend is not
/// available yet.
pub struct NoopClassifier;

impl Classifier for NoopClassifier {
    fn suggest(&self, _email: &Email) -> Option<Suggestion> {
        None
    }
}

/// Build the runtime classifier from the `[ai]` config block. Phase 5.a
/// always returns `NoopClassifier`; Phase 6 will branch on
/// `config.enabled` / `config.backend` to load the real embeddings model.
/// Returning an `Arc` lets AppRoot share the same instance with
/// MessagesComponent without re-instantiating per render.
pub fn build_classifier(_config: &AiConfig) -> Arc<dyn Classifier> {
    Arc::new(NoopClassifier)
}

/// Single-character glyph for a suggestion's action — matches the
/// default keymap binding so users can read the chip as "press this key
/// to accept" (`a`=Archive, `s`=Star, `d`=Delete, `r`=ReplyAll, etc.).
/// Returns `None` for actions that have no meaningful one-key shortcut
/// in the Messages-pane context.
pub fn suggestion_glyph(action: Action) -> Option<char> {
    match action {
        Action::Archive => Some('a'),
        Action::Star => Some('s'),
        Action::Delete => Some('d'),
        Action::ReplyAll => Some('r'),
        Action::Forward => Some('f'),
        Action::MarkUnread => Some('U'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn email() -> Email {
        Email::new(PathBuf::from("/tmp/m"))
    }

    /// VISION.md §AI Classifier: with `[ai].enabled = false`, the chip
    /// must never render and `;` must be a no-op. `NoopClassifier` is the
    /// mechanism — always-`None` keeps the render path cheap and the
    /// accept-key path quiet.
    #[test]
    fn noop_classifier_returns_none_for_any_email() {
        let c = NoopClassifier;
        assert_eq!(c.suggest(&email()), None);
    }

    /// Default config (`enabled = false`) wires up the no-op. Phase 6
    /// will start returning a real backend when `enabled = true`; the
    /// stub keeps the contract by always returning `NoopClassifier`.
    #[test]
    fn build_classifier_returns_noop_for_default_config() {
        let cfg = AiConfig::default();
        let c = build_classifier(&cfg);
        assert!(c.suggest(&email()).is_none());
    }

    /// Even with `enabled = true`, Phase 5.a still hands back the
    /// no-op — the real model lands in Phase 6. Asserting this here
    /// makes the placeholder explicit so a future change to
    /// `build_classifier` updates the test in lockstep.
    #[test]
    fn build_classifier_is_noop_even_when_enabled_in_phase_5a() {
        let cfg = AiConfig {
            enabled: true,
            ..AiConfig::default()
        };
        let c = build_classifier(&cfg);
        assert!(c.suggest(&email()).is_none());
    }

    /// Chip glyph mirrors the default keymap binding for each
    /// suggested action — the user reads the chip and knows which key
    /// already accepts it via the existing keymap (in addition to `;`).
    #[test]
    fn suggestion_glyph_matches_default_keymap() {
        assert_eq!(suggestion_glyph(Action::Archive), Some('a'));
        assert_eq!(suggestion_glyph(Action::Star), Some('s'));
        assert_eq!(suggestion_glyph(Action::Delete), Some('d'));
        assert_eq!(suggestion_glyph(Action::ReplyAll), Some('r'));
        assert_eq!(suggestion_glyph(Action::Forward), Some('f'));
        // Actions without a single-key chip slot return None — the
        // chip is left blank rather than showing a misleading char.
        assert_eq!(suggestion_glyph(Action::MoveDown), None);
    }
}
