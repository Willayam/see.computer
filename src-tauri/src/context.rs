//! Short-lived words visible to the paste target.

use std::sync::mpsc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub text: String,
    pub source: &'static str,
}

/// The receiver is polled by the engine. An AX timeout never holds up paste.
pub fn begin() -> mpsc::Receiver<Vec<Term>> {
    let (tx, rx) = mpsc::channel();
    crate::qos::spawn("see-screen-context", crate::qos::Class::Upkeep, move || {
        let terms = if std::env::var("SEE_COMPUTER_SCREEN_CONTEXT").ok().as_deref() == Some("0") {
            Vec::new()
        } else {
            crate::readback::screen_text()
                .map(|text| crate::learn::context_terms(&text, &crate::learn::Words::system()))
                .unwrap_or_default()
                .into_iter()
                .map(|text| Term { text, source: "screen" })
                .collect()
        };
        let _ = tx.send(terms);
    });
    rx
}

#[cfg(test)]
mod tests {
    #[test]
    fn screen_term_can_land_only_in_the_boosted_hunk() {
        let words = crate::learn::Words::from_list(&["use", "the", "project"]);
        let extracted = crate::learn::context_terms("use the Zorvia project", &words);
        assert_eq!(extracted, ["Zorvia"]);
        let terms = extracted.into_iter().map(|text| crate::boost::Term { text, weight: 0.5 }).collect::<Vec<_>>();
        assert_eq!(crate::boost::merge("Use the Sylvia project", "Use the Zorvia project", &terms), "Use the Zorvia project");
        assert_eq!(crate::boost::merge("Use the Sylvia project", "Use the Zorvia project", &[]), "Use the Sylvia project");
    }
}
