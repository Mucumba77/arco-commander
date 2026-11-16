use super::*;

#[derive(Clone)]
pub(super) struct Partner {
    extensions: Vec<String>,
    missing: bool,
}

impl Partner {
    pub(super) fn compile(text: &str, missing: bool) -> io::Result<Option<Self>> {
        if text.trim().is_empty() {
            return Ok(None);
        }
        let mut extensions = Vec::new();
        for extension in text.split(';') {
            let extension = extension.trim().trim_start_matches('.');
            if extension.is_empty()
                || extension.len() > 255
                || extension
                    .chars()
                    .any(|ch| ch.is_control() || r#"/\:*?"<>|"#.contains(ch))
            {
                return Err(io::Error::other(
                    "Gib Partner-Erweiterungen ohne Pfad oder Wildcards ein, z. B. jpg;png.",
                ));
            }
            if !extensions
                .iter()
                .any(|value: &String| value.eq_ignore_ascii_case(extension))
            {
                extensions.push(extension.to_owned());
            }
        }
        if extensions.len() > 32 {
            return Err(io::Error::other(
                "Wähle höchstens 32 Partner-Erweiterungen.",
            ));
        }
        Ok(Some(Self {
            extensions,
            missing,
        }))
    }
}

impl Query {
    pub fn has_partner_filter(&self) -> bool {
        self.partner.is_some()
    }

    pub fn matches_partner(
        &self,
        entry: &Entry,
        mut exists: impl FnMut(&Path) -> io::Result<bool>,
    ) -> io::Result<bool> {
        let Some(partner) = &self.partner else {
            return Ok(true);
        };
        if entry.kind != Kind::File {
            return Ok(false);
        }
        let mut found = false;
        for extension in &partner.extensions {
            // A file is never its own partner, even on a case-insensitive filesystem.
            if entry
                .path
                .extension()
                .is_some_and(|own| own.to_string_lossy().eq_ignore_ascii_case(extension))
            {
                continue;
            }
            let candidate = entry.path.with_extension(extension);
            if exists(&candidate)? {
                found = true;
                break;
            }
        }
        Ok(found != partner.missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partner_extension_search_supports_profiles_inverse_filters_and_same_name_in_same_folder() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        for name in ["one.cr2", "one.jpg", "two.cr2", "three.cr2", "three.png"] {
            fs::write(root.join(name), b"image").unwrap();
        }
        fs::create_dir(root.join("two.jpg")).unwrap();
        let options = Options {
            names: "*.cr2".into(),
            partner_extensions: ".jpg;png".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&options).unwrap();
        let mut options: Options = serde_json::from_value(json).unwrap();
        let matches = |options: &Options| {
            let found = search(
                &[root.to_path_buf()],
                false,
                &options.compile().unwrap(),
                true,
                None,
                &AtomicBool::new(false),
            );
            assert!(
                found.directory.errors.is_empty(),
                "{:?}",
                found.directory.errors
            );
            found
                .directory
                .entries
                .iter()
                .map(|entry| entry.name.to_string_lossy().into_owned())
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            matches(&options),
            ["one.cr2".into(), "three.cr2".into()].into()
        );
        options.partner_not = true;
        assert_eq!(matches(&options), ["two.cr2".into()].into());
        options.partner_not = false;
        options.partner_extensions = "cr2".into();
        assert!(matches(&options).is_empty());
        for extension in ["../jpg", "*.jpg", "jpg;;png", "jpg\\file"] {
            options.partner_extensions = extension.into();
            assert!(options.compile().is_err());
        }
    }
}
