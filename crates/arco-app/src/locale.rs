use arco_core::profile::Language;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::OnceLock;

thread_local! {
    static LANGUAGE: Cell<Language> = const { Cell::new(Language::German) };
}

pub const LANGUAGES: [Language; 3] = [Language::German, Language::English, Language::French];

pub fn label(language: Language) -> &'static str {
    match language {
        Language::German => "Deutsch",
        Language::English => "English",
        Language::French => "Français",
    }
}

pub fn select(language: Language) -> Result<(), slint::SelectBundledTranslationError> {
    slint::select_bundled_translation(match language {
        Language::German => "",
        Language::English => "en-US",
        Language::French => "fr-FR",
    })?;
    LANGUAGE.set(language);
    Ok(())
}

pub fn tr(text: &str) -> &str {
    translate(LANGUAGE.get(), text)
}

fn translate(language: Language, text: &str) -> &str {
    static ENGLISH: OnceLock<HashMap<&str, &str>> = OnceLock::new();
    static FRENCH: OnceLock<HashMap<&str, &str>> = OnceLock::new();
    let (translations, catalog) = match language {
        Language::German => return text,
        Language::English => (&ENGLISH, include_str!("../i18n/en.tsv")),
        Language::French => (&FRENCH, include_str!("../i18n/fr.tsv")),
    };
    translations
        .get_or_init(|| {
            catalog
                .lines()
                .filter_map(|line| line.split_once('\t'))
                .collect()
        })
        .get(text)
        .copied()
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_the_interface_and_retains_unknown_text() {
        assert_eq!(translate(Language::German, "Dateien"), "Dateien");
        assert_eq!(translate(Language::English, "Dateien"), "Files");
        assert_eq!(translate(Language::French, "Dateien"), "Fichiers");
        assert_eq!(translate(Language::French, "Werkzeuge"), "Outils");
        assert_eq!(translate(Language::French, "C:\\temp"), "C:\\temp");
    }
}
