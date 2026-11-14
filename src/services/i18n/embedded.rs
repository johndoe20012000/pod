//! Pod's translations, embedded as text and parsed the first time a locale is asked for.
//!
//! The `i18n!` macro can load the locale files itself, but it expands them into one generated function that inserts
//! every string into a map. With nine locales of about 3,500 keys each, optimizing that function in a release build
//! needs more than 14 GB of memory. Embedded as text, the strings reach the compiler as nine static byte strings; the
//! macro is pointed at a directory without locale files, so it generates no table of its own.

use std::{borrow::Cow, collections::HashMap, sync::OnceLock};

use rust_i18n::Backend;

const LOCALES: [(&str, &str); 9] = [
  ("de", include_str!("../../../assets/locales/de.toml")),
  ("en", include_str!("../../../assets/locales/en.toml")),
  ("en-us", include_str!("../../../assets/locales/en-us.toml")),
  ("es", include_str!("../../../assets/locales/es.toml")),
  ("fr", include_str!("../../../assets/locales/fr.toml")),
  ("ja", include_str!("../../../assets/locales/ja.toml")),
  ("ko", include_str!("../../../assets/locales/ko.toml")),
  ("ru", include_str!("../../../assets/locales/ru.toml")),
  ("zh", include_str!("../../../assets/locales/zh.toml")),
];

#[derive(Default)]
pub struct EmbeddedLocales {
  messages: [OnceLock<HashMap<String, String>>; LOCALES.len()],
}

impl EmbeddedLocales {
  fn messages(&self, locale: &str) -> Option<&HashMap<String, String>> {
    let index = LOCALES.iter().position(|(name, _)| *name == locale)?;
    Some(self.messages[index].get_or_init(|| parse(LOCALES[index].1)))
  }
}

impl Backend for EmbeddedLocales {
  fn available_locales(&self) -> Vec<Cow<'_, str>> {
    LOCALES.iter().map(|(locale, _)| Cow::Borrowed(*locale)).collect()
  }

  fn messages_for_locale(&self, locale: &str) -> Option<Vec<(Cow<'_, str>, Cow<'_, str>)>> {
    let messages = self.messages(locale)?;
    Some(
      messages
        .iter()
        .map(|(key, text)| (Cow::Borrowed(key.as_str()), Cow::Borrowed(text.as_str())))
        .collect(),
    )
  }

  fn translate(&self, locale: &str, key: &str) -> Option<Cow<'_, str>> {
    self.messages(locale)?.get(key).map(|text| Cow::Borrowed(text.as_str()))
  }
}

fn flatten(key: String, value: toml::Value, messages: &mut HashMap<String, String>) {
  match value {
    toml::Value::String(text) => {
      messages.insert(key, text);
    }
    toml::Value::Table(table) => {
      for (child, value) in table {
        flatten(format!("{key}.{child}"), value, messages);
      }
    }
    // Pod's locale files hold only strings; anything else keeps its TOML text rather than vanishing.
    other => {
      messages.insert(key, other.to_string());
    }
  }
}

/// Panics on malformed TOML: the files are compiled in, and `tests/i18n_parity.rs` parses every one of them.
fn parse(source: &str) -> HashMap<String, String> {
  let table: toml::Table = toml::from_str(source).expect("embedded locale files are valid TOML");
  let mut messages = HashMap::new();
  for (key, value) in table {
    flatten(key, value, &mut messages);
  }
  messages
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeSet;

  use super::*;

  fn keys(locales: &EmbeddedLocales, locale: &str) -> BTreeSet<String> {
    locales
      .messages_for_locale(locale)
      .unwrap_or_else(|| panic!("{locale} is embedded"))
      .into_iter()
      .map(|(key, _)| key.into_owned())
      .collect()
  }

  mod available_locales {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn it_lists_every_locale_file() {
      let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/locales");
      let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .filter_map(|entry| {
          let path = entry.ok()?.path();
          if path.extension()? != "toml" {
            return None;
          }
          Some(path.file_stem()?.to_str()?.to_owned())
        })
        .collect();
      on_disk.sort();

      assert_eq!(EmbeddedLocales::default().available_locales(), on_disk);
    }
  }

  mod messages_for_locale {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn it_gives_every_locale_the_keys_of_en() {
      let locales = EmbeddedLocales::default();
      let en = keys(&locales, "en");

      assert!(en.contains("shell.notifications.title"));
      for locale in locales.available_locales() {
        assert_eq!(keys(&locales, &locale), en, "{locale}");
      }
    }

    #[test]
    fn it_returns_none_for_an_unknown_locale() {
      assert!(EmbeddedLocales::default().messages_for_locale("xx").is_none());
    }
  }

  mod translate {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn it_joins_nested_tables_into_a_dotted_key() {
      let locales = EmbeddedLocales::default();

      assert_eq!(
        locales.translate("en", "shell.notifications.title").as_deref(),
        Some("Notifications")
      );
    }

    #[test]
    fn it_keeps_placeholders_for_interpolation() {
      let locales = EmbeddedLocales::default();

      assert_eq!(
        locales.translate("en", "shell.notifications.footer_unread").as_deref(),
        Some("%{count} unread")
      );
    }

    #[test]
    fn it_returns_none_for_an_unknown_key() {
      assert!(
        EmbeddedLocales::default()
          .translate("en", "totally.absent.key")
          .is_none()
      );
    }

    #[test]
    fn it_returns_none_for_an_unknown_locale() {
      assert!(
        EmbeddedLocales::default()
          .translate("xx", "shell.notifications.title")
          .is_none()
      );
    }

    #[test]
    fn it_returns_the_text_of_the_requested_locale() {
      let locales = EmbeddedLocales::default();

      assert_eq!(
        locales.translate("de", "shell.notifications.title").as_deref(),
        Some("Benachrichtigungen")
      );
    }
  }
}
