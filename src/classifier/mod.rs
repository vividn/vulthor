// AI classifier interface. Phase 5.a shipped the trait + NoopClassifier
// disabled-by-default. Phase 6.a (vu-po7) layers an embedding-backed
// classifier behind the optional `ai` feature: a k-NN over a labelled
// index of subject+body embeddings, gated at runtime by `[ai].enabled`
// and the presence of an ONNX model file. Default builds (no feature)
// still get NoopClassifier and pay none of the fastembed/ort cost.

use std::sync::Arc;

use crate::config::AiConfig;
use crate::email::Email;
use crate::keymap::Action;

#[cfg(feature = "ai")]
pub mod embedding;

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
/// `[ai].enabled = false` (the default), the `ai` feature is not
/// compiled in, or model/index loading fails at startup.
pub struct NoopClassifier;

impl Classifier for NoopClassifier {
    fn suggest(&self, _email: &Email) -> Option<Suggestion> {
        None
    }
}

/// Build the runtime classifier from the `[ai]` config block.
///
/// Order of precedence (highest first):
/// 1. `ai` feature not compiled in → `NoopClassifier`.
/// 2. `config.enabled = false` → `NoopClassifier`.
/// 3. `config.backend != "embeddings"` → `NoopClassifier`.
/// 4. `config.model_path` missing/unset or model fails to load →
///    `NoopClassifier` (with a stderr warning).
/// 5. Otherwise → `EmbeddingClassifier` with the on-disk index loaded
///    from `~/.local/share/vulthor/classifier.idx` (empty if the file
///    does not exist yet — predictions return `None` until trained).
pub fn build_classifier(config: &AiConfig) -> Arc<dyn Classifier> {
    if !config.enabled {
        return Arc::new(NoopClassifier);
    }

    #[cfg(feature = "ai")]
    {
        if config.backend == "embeddings" {
            match embedding::build(config) {
                Ok(c) => return Arc::new(c),
                Err(e) => {
                    eprintln!(
                        "AI classifier disabled: failed to initialize embeddings backend ({e}). Falling back to NoopClassifier."
                    );
                }
            }
        }
    }
    let _ = config; // suppress unused warning when feature off
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

    /// Default config (`enabled = false`) wires up the no-op regardless
    /// of feature compilation state.
    #[test]
    fn build_classifier_returns_noop_for_default_config() {
        let cfg = AiConfig::default();
        let c = build_classifier(&cfg);
        assert!(c.suggest(&email()).is_none());
    }

    /// Without the `ai` feature, even `enabled = true` resolves to the
    /// no-op — the feature gate is the hard guard. With the feature on
    /// but no `model_path`, the embeddings builder errors and we fall
    /// back to NoopClassifier (covered separately in the embedding
    /// module tests). Either way the public contract is the same.
    #[test]
    fn build_classifier_is_noop_without_model_path() {
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
