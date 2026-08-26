//! Rescoring hook for the transducer's joint logits during greedy decoding.
//!
//! Shallow fusion in the sense NeMo's word boosting uses: the decoder still
//! runs greedily, but a caller-supplied bias nudges the vocabulary scores at
//! every step, so terms the acoustic model is unsure about can be made to win.

/// Adjusts the vocabulary logits of one decoding step, before the argmax.
///
/// One instance decodes one utterance. [`reset`](TokenBias::reset) is called
/// before the first frame, [`bias`](TokenBias::bias) once per decoding step,
/// and [`emitted`](TokenBias::emitted) for every non-blank token the decoder
/// commits to, which is how an implementation tracks where it is inside a
/// multi-token phrase.
pub trait TokenBias {
    /// Start of an utterance. Drop any state carried over from the last one.
    fn reset(&mut self) {}

    /// Add per-token bonuses or penalties in place.
    ///
    /// `logits` covers the vocabulary only. TDT's trailing duration logits are
    /// a separate distribution and are not passed here: biasing them would
    /// change how many frames the decoder skips, not which token it picks.
    fn bias(&mut self, logits: &mut [f32]);

    /// The decoder committed to `token`. Blanks are not reported.
    fn emitted(&mut self, token: usize);
}
