// Phase 6.a embedding-backed classifier. The runtime side is gated by
// the optional `ai` Cargo feature so a default `cargo build` does not
// pull fastembed / ort. The shape:
//
//   text ──► Embedder (fastembed, locally-loaded ONNX) ──► Vec<f32>
//                                                            │
//                                            cosine k-NN ◄───┘
//                                                  │
//                                  IndexEntry { vector, action } ──► Suggestion
//
// The labelled `KnnIndex` is loaded once at startup from
// `~/.local/share/vulthor/classifier.idx` (JSON). A missing file is
// not an error — the index is just empty and every prediction returns
// `None` until the user trains one. Persistence is JSON rather than
// bincode so the file is hand-inspectable and survives schema drift
// gracefully.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::classifier::{Classifier, Suggestion};
use crate::config::AiConfig;
use crate::email::Email;
use crate::keymap::Action;

/// Pluggable text-to-embedding backend. Trait-object form so the
/// classifier can carry a `Box<dyn Embedder>` and tests can supply a
/// deterministic stub without needing a real ONNX runtime.
pub trait Embedder: Send + Sync {
    /// Embed `text` into a fixed-dimension vector. Returning an error
    /// here is treated as "no prediction" at the call site, not a panic.
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let _ = text;
        Err(EmbedError::new("embedder did not implement `embed`"))
    }
}

/// Opaque embedder error — surfaced into the structured
/// [`EmbeddingClassifierError`] when build- or run-time embedding
/// fails. Stringly-typed because backends (fastembed/ort/candle) all
/// carry their own incompatible error hierarchies.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct EmbedError(String);

impl EmbedError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// One labelled exemplar in the k-NN index. Action is stored as the
/// canonical [`Action::name`] string so the on-disk index survives
/// enum reorderings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// L2-normalized embedding vector. The classifier renormalizes on
    /// load so user-edited index files do not need to be exact.
    pub vector: Vec<f32>,
    /// Canonical [`Action`] name (see [`Action::name`]).
    pub action: String,
}

/// In-memory labelled embedding index used for cosine k-NN lookups.
/// Empty after a missing-file load — predictions return `None` until
/// it is populated.
#[derive(Debug, Default, Clone)]
pub struct KnnIndex {
    entries: Vec<(Vec<f32>, Action)>,
}

impl KnnIndex {
    /// Build an empty index. Used when the on-disk file is missing or
    /// when constructing fixtures in tests.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Insert a labelled exemplar. Vectors are normalized on insert so
    /// the query path can assume unit-length vectors.
    pub fn insert(&mut self, vector: Vec<f32>, action: Action) {
        let v = normalize(vector);
        self.entries.push((v, action));
    }

    /// Number of labelled exemplars in the index.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` iff the index has no labelled exemplars — the query path
    /// short-circuits to `None`.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Nearest-neighbour lookup by cosine similarity. Returns the
    /// best-matching `(action, similarity)` or `None` for an empty
    /// index. Similarity is in `[-1.0, 1.0]`; the caller compares
    /// against `[ai].threshold`.
    pub fn nearest(&self, query: &[f32]) -> Option<(Action, f32)> {
        if self.entries.is_empty() {
            return None;
        }
        let q = normalize(query.to_vec());
        let mut best: Option<(Action, f32)> = None;
        for (v, a) in &self.entries {
            let s = dot(&q, v);
            match &best {
                Some((_, bs)) if *bs >= s => {}
                _ => best = Some((*a, s)),
            }
        }
        best
    }

    /// Load from a JSON file. A missing file yields an empty index
    /// rather than an error — the user has not trained yet, and that
    /// is a valid state.
    pub fn load_from_file(path: &Path) -> Result<Self, EmbeddingClassifierError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::empty()),
            Err(e) => return Err(EmbeddingClassifierError::IndexIo(e)),
        };
        let raw: Vec<IndexEntry> = serde_json::from_slice(&bytes)
            .map_err(|e| EmbeddingClassifierError::IndexParse(e.to_string()))?;
        let mut idx = Self::empty();
        for entry in raw {
            let Some(action) = Action::from_name(&entry.action) else {
                return Err(EmbeddingClassifierError::IndexParse(format!(
                    "unknown action {:?} in index",
                    entry.action
                )));
            };
            idx.insert(entry.vector, action);
        }
        Ok(idx)
    }

    /// Serialize to a JSON file. Parent directories are created if
    /// missing. Used by future training flows; not invoked at startup.
    pub fn save_to_file(&self, path: &Path) -> Result<(), EmbeddingClassifierError> {
        let entries: Vec<IndexEntry> = self
            .entries
            .iter()
            .map(|(v, a)| IndexEntry {
                vector: v.clone(),
                action: a.name().to_string(),
            })
            .collect();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(EmbeddingClassifierError::IndexIo)?;
        }
        let bytes = serde_json::to_vec_pretty(&entries)
            .map_err(|e| EmbeddingClassifierError::IndexParse(e.to_string()))?;
        fs::write(path, bytes).map_err(EmbeddingClassifierError::IndexIo)
    }
}

/// Errors that can be returned by [`build`] or by the lower-level
/// index/embedder paths. The classifier-builder caller (`build_classifier`
/// in the parent module) logs these and falls back to `NoopClassifier`,
/// so this type exists primarily for diagnostics rather than recovery.
#[derive(Debug, Error)]
pub enum EmbeddingClassifierError {
    /// `[ai].model_path` was unset; without an ONNX file there is no
    /// way to embed text.
    #[error("[ai].model_path is not set")]
    ModelPathUnset,
    /// I/O failure while reading the labelled index file.
    #[error("index I/O error: {0}")]
    IndexIo(#[source] io::Error),
    /// The index file existed but could not be parsed (bad JSON,
    /// unknown action name, etc).
    #[error("index parse error: {0}")]
    IndexParse(String),
    /// Embedder construction failed (model file missing or backend
    /// rejected it).
    #[error("embedder init failed: {0}")]
    EmbedderInit(String),
}

/// Real embedding-backed classifier. Holds the embedder, the labelled
/// k-NN index, the confidence threshold, and the maximum number of
/// body characters to embed.
pub struct EmbeddingClassifier {
    embedder: Box<dyn Embedder>,
    index: KnnIndex,
    threshold: f32,
    body_chars: usize,
}

/// Default body-text prefix length fed to the embedder. VISION.md /
/// the bead spec call out "subject + first 512 chars of body". Pulled
/// out as a constant so tests and future tuning can reference it.
const DEFAULT_BODY_CHARS: usize = 512;

impl EmbeddingClassifier {
    /// Wire a classifier together from its parts. Public so tests can
    /// construct one with a stub embedder + handcrafted index without
    /// touching the on-disk model.
    pub fn new(embedder: Box<dyn Embedder>, index: KnnIndex, threshold: f32) -> Self {
        Self {
            embedder,
            index,
            threshold,
            body_chars: DEFAULT_BODY_CHARS,
        }
    }

    /// Subject + body-prefix concatenation used as embedder input. A
    /// single newline separator keeps the two fields token-distinct
    /// without leaning on backend-specific special tokens.
    fn email_text(&self, email: &Email) -> String {
        let mut s = String::with_capacity(email.headers.subject.len() + self.body_chars + 1);
        s.push_str(&email.headers.subject);
        s.push('\n');
        let body = &email.body_text;
        let take = body.char_indices().nth(self.body_chars).map(|(i, _)| i);
        let slice = match take {
            Some(end) => &body[..end],
            None => body.as_str(),
        };
        s.push_str(slice);
        s
    }
}

impl Classifier for EmbeddingClassifier {
    fn suggest(&self, email: &Email) -> Option<Suggestion> {
        if self.index.is_empty() {
            return None;
        }
        let text = self.email_text(email);
        let vector = match self.embedder.embed(&text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("classifier embed failed: {e}");
                return None;
            }
        };
        let (action, similarity) = self.index.nearest(&vector)?;
        if similarity >= self.threshold {
            Some(Suggestion {
                action,
                confidence: similarity,
            })
        } else {
            None
        }
    }
}

/// Build the runtime embedding classifier from `[ai]` config. Returns
/// `Err` when the model path is unset or the model / index fail to
/// load — the parent `build_classifier` logs and falls back to
/// [`crate::classifier::NoopClassifier`].
pub fn build(config: &AiConfig) -> Result<EmbeddingClassifier, EmbeddingClassifierError> {
    let model_path = config
        .model_path
        .as_ref()
        .ok_or(EmbeddingClassifierError::ModelPathUnset)?;
    let embedder = build_embedder(model_path)?;
    let index = KnnIndex::load_from_file(&default_index_path())?;
    Ok(EmbeddingClassifier::new(embedder, index, config.threshold))
}

/// XDG-default index location. Lives next to other Vulthor state under
/// `~/.local/share/vulthor/`. Falls back to `./classifier.idx` if XDG
/// resolution fails (rare, but keeps tests / CI hermetic).
pub fn default_index_path() -> PathBuf {
    if let Some(dir) = dirs::data_dir() {
        dir.join("vulthor").join("classifier.idx")
    } else {
        PathBuf::from("classifier.idx")
    }
}

/// L2 norm. Pulled out so the test path can exercise it directly.
fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    v
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut s = 0.0;
    for i in 0..n {
        s += a[i] * b[i];
    }
    s
}

// --- fastembed-backed embedder (feature-gated) -----------------------

#[cfg(feature = "ai")]
fn build_embedder(model_path: &Path) -> Result<Box<dyn Embedder>, EmbeddingClassifierError> {
    FastEmbedEmbedder::from_path(model_path)
        .map(|e| Box::new(e) as Box<dyn Embedder>)
        .map_err(|e| EmbeddingClassifierError::EmbedderInit(e.to_string()))
}

#[cfg(not(feature = "ai"))]
fn build_embedder(_model_path: &Path) -> Result<Box<dyn Embedder>, EmbeddingClassifierError> {
    // Unreachable in practice — `build_classifier` only calls into
    // this module when `feature = "ai"` — but kept defined so the
    // module compiles on its own under either feature setting.
    Err(EmbeddingClassifierError::EmbedderInit(
        "fastembed feature not compiled in".into(),
    ))
}

#[cfg(feature = "ai")]
mod fastembed_backend {
    use std::path::Path;

    use fastembed::{
        EmbeddingModel, InitOptions, TextEmbedding, UserDefinedEmbeddingModel,
        read_file_to_bytes,
    };

    use super::{EmbedError, Embedder};

    /// Wraps a locally-loaded fastembed `TextEmbedding`. The model file
    /// (.onnx) is opened from `[ai].model_path`; the tokenizer files
    /// are expected to sit alongside it (`tokenizer.json`, etc).
    pub struct FastEmbedEmbedder {
        inner: TextEmbedding,
    }

    impl FastEmbedEmbedder {
        /// Try to construct from a user-supplied ONNX model path.
        /// Looks for a `tokenizer.json` next to the model and bails
        /// with a structured error if either file is missing.
        pub fn from_path(model_path: &Path) -> Result<Self, EmbedError> {
            // Fast path: if the path looks like a built-in fastembed
            // model name (no separator, no `.onnx`), let fastembed
            // resolve it via its bundled list.
            if model_path.extension().is_none() && model_path.components().count() == 1 {
                let name = model_path.to_string_lossy().to_string();
                let model = EmbeddingModel::AllMiniLML6V2; // sensible default
                let _ = name; // user-named built-ins not wired yet
                let inner = TextEmbedding::try_new(
                    InitOptions::new(model).with_show_download_progress(false),
                )
                .map_err(|e| EmbedError::new(e.to_string()))?;
                return Ok(Self { inner });
            }

            let parent = model_path.parent().ok_or_else(|| {
                EmbedError::new(format!(
                    "model path {:?} has no parent directory",
                    model_path
                ))
            })?;
            let tokenizer = parent.join("tokenizer.json");
            let config = parent.join("config.json");
            let special_tokens = parent.join("special_tokens_map.json");
            let tokenizer_config = parent.join("tokenizer_config.json");

            let onnx_bytes =
                read_file_to_bytes(model_path).map_err(|e| EmbedError::new(e.to_string()))?;
            let tokenizer_bytes =
                read_file_to_bytes(&tokenizer).map_err(|e| EmbedError::new(e.to_string()))?;
            let config_bytes =
                read_file_to_bytes(&config).map_err(|e| EmbedError::new(e.to_string()))?;
            let special_tokens_bytes = read_file_to_bytes(&special_tokens)
                .map_err(|e| EmbedError::new(e.to_string()))?;
            let tokenizer_config_bytes = read_file_to_bytes(&tokenizer_config)
                .map_err(|e| EmbedError::new(e.to_string()))?;

            let model = UserDefinedEmbeddingModel::new(onnx_bytes, fastembed::TokenizerFiles {
                tokenizer_file: tokenizer_bytes,
                config_file: config_bytes,
                special_tokens_map_file: special_tokens_bytes,
                tokenizer_config_file: tokenizer_config_bytes,
            });
            let inner = TextEmbedding::try_new_from_user_defined(model, Default::default())
                .map_err(|e| EmbedError::new(e.to_string()))?;
            Ok(Self { inner })
        }
    }

    impl Embedder for FastEmbedEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
            let mut out = self
                .inner
                .embed(vec![text], None)
                .map_err(|e| EmbedError::new(e.to_string()))?;
            out.pop()
                .ok_or_else(|| EmbedError::new("fastembed returned no vectors"))
        }
    }
}

#[cfg(feature = "ai")]
pub use fastembed_backend::FastEmbedEmbedder;

// Silence "unused" when the parent module imports `Arc` only on some
// paths. `Arc` is used by `build_classifier` in `mod.rs`; this module
// itself does not need it.
#[allow(dead_code)]
fn _arc_marker() -> Option<Arc<EmbeddingClassifier>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Deterministic stub embedder for tests. Maps a small handful of
    /// substrings to fixed unit vectors so we can wire up a tiny k-NN
    /// index and assert exact outputs — no ONNX runtime, no network.
    struct StubEmbedder;

    impl Embedder for StubEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
            // Three orthogonal axes: archive / star / delete. Falls
            // back to a neutral vector so below-threshold tests have
            // a stable "nothing matches well" input.
            if text.contains("archive-me") {
                Ok(vec![1.0, 0.0, 0.0])
            } else if text.contains("star-me") {
                Ok(vec![0.0, 1.0, 0.0])
            } else if text.contains("delete-me") {
                Ok(vec![0.0, 0.0, 1.0])
            } else {
                Ok(vec![0.5, 0.5, 0.5])
            }
        }
    }

    fn make_email(subject: &str, body: &str) -> Email {
        let mut e = Email::new(PathBuf::from("/tmp/m"));
        e.headers.subject = subject.to_string();
        e.body_text = body.to_string();
        e
    }

    fn make_index() -> KnnIndex {
        let mut idx = KnnIndex::empty();
        idx.insert(vec![1.0, 0.0, 0.0], Action::Archive);
        idx.insert(vec![0.0, 1.0, 0.0], Action::Star);
        idx.insert(vec![0.0, 0.0, 1.0], Action::Delete);
        idx
    }

    /// With a populated index, embedding text that lands on an axis
    /// should produce a confidence near 1.0 for that label.
    #[test]
    fn classifier_returns_high_confidence_suggestion_for_known_text() {
        let c = EmbeddingClassifier::new(Box::new(StubEmbedder), make_index(), 0.9);
        let s = c.suggest(&make_email("hello", "archive-me please")).unwrap();
        assert_eq!(s.action, Action::Archive);
        assert!(
            s.confidence > 0.99,
            "expected confidence near 1.0, got {}",
            s.confidence
        );
    }

    /// A neutral input is roughly equidistant from all axes (≈0.577
    /// cosine similarity) — below a 0.9 threshold, so the classifier
    /// must abstain.
    #[test]
    fn classifier_returns_none_below_threshold() {
        let c = EmbeddingClassifier::new(Box::new(StubEmbedder), make_index(), 0.9);
        assert!(c.suggest(&make_email("subj", "boring body")).is_none());
    }

    /// An empty index = freshly initialised classifier with no labels
    /// = always-`None`. This is the bootstrap state until the user
    /// trains.
    #[test]
    fn empty_index_yields_no_predictions() {
        let c = EmbeddingClassifier::new(Box::new(StubEmbedder), KnnIndex::empty(), 0.5);
        assert!(c.suggest(&make_email("anything", "archive-me")).is_none());
    }

    /// Round-trip: save + load preserves entries and labels. Catches
    /// serializer regressions and proves the on-disk format is
    /// stable.
    #[test]
    fn index_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("classifier.idx");
        let mut idx = KnnIndex::empty();
        idx.insert(vec![3.0, 4.0, 0.0], Action::Archive);
        idx.insert(vec![0.0, 0.0, 7.0], Action::Delete);
        idx.save_to_file(&path).unwrap();

        let loaded = KnnIndex::load_from_file(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        let (action, sim) = loaded.nearest(&[1.0, 0.0, 0.0]).unwrap();
        assert_eq!(action, Action::Archive);
        assert!(sim > 0.5);
    }

    /// A missing index file is the bootstrap state — load returns an
    /// empty index rather than erroring out. Predictions then short-
    /// circuit to `None` at the classifier level.
    #[test]
    fn missing_index_file_loads_as_empty() {
        let idx = KnnIndex::load_from_file(Path::new("/tmp/vulthor-nonexistent-idx.json")).unwrap();
        assert!(idx.is_empty());
    }

    /// `build()` must refuse to construct a real classifier when no
    /// model path is configured — without that the embedder cannot be
    /// initialised. The outer `build_classifier` then falls back to
    /// `NoopClassifier`.
    #[test]
    fn build_errors_when_model_path_unset() {
        let cfg = AiConfig {
            enabled: true,
            ..AiConfig::default()
        };
        let err = build(&cfg).unwrap_err();
        assert!(matches!(err, EmbeddingClassifierError::ModelPathUnset));
    }
}
