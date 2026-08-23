use parakeet_rs::Transcriber;

use super::{Engine, EngineError, ModelFiles};
use crate::mic::Audio16k;
use crate::paste::Text;

pub struct Parakeet {
    model: parakeet_rs::ParakeetTDT,
}

impl Parakeet {
    pub fn load(files: &ModelFiles) -> Result<Parakeet, EngineError> {
        parakeet_rs::ParakeetTDT::from_pretrained(&files.dir, None)
            .map(|model| Parakeet { model })
            .map_err(|error| EngineError::Load(error.to_string()))
    }
}

impl Engine for Parakeet {
    fn transcribe(&mut self, audio: &Audio16k) -> Result<Option<Text>, EngineError> {
        self.model
            .transcribe_samples(audio.samples().to_vec(), 16_000, 1, None)
            .map(|result| Text::parse(result.text))
            .map_err(|error| EngineError::Inference(error.to_string()))
    }
}
