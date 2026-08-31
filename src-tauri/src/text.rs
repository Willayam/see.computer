//! A non-empty, trimmed string, the only shape the app pastes.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text(String);

impl Text {
    pub fn parse(raw: impl Into<String>) -> Option<Text> {
        let text = raw.into().trim().to_owned();
        (!text.is_empty()).then_some(Text(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn literal(raw: String) -> Text {
        Text(raw)
    }

    /// Dictated sentences land mid-document; a trailing space keeps the next
    /// one from gluing onto this one.
    pub fn followed_by_space(mut self) -> Text {
        self.0.push(' ');
        self
    }
}

#[cfg(test)]
mod tests {
    use super::Text;

    #[test]
    fn text_is_trimmed_and_non_empty() {
        assert_eq!(
            Text::parse("  hello \n").map(|text| text.0),
            Some("hello".to_owned())
        );
        assert!(Text::parse("  ").is_none());
    }
}
