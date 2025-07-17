use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing field: {0}")]
    MissingField(&'static str),
    #[error("parse error: {0}")]
    ParseError(Box<dyn std::error::Error>),
    #[error("expected field {0}, got {1}")]
    MismatchedField(&'static str, String),
}
type Result<T> = std::result::Result<T, Error>;

/// The contents of `.XComMod` files.
pub struct ModMetadata {
    pub published_file_id: u32,
    pub title: String,
    pub description: String,
    pub requires_xpack: bool,
    /// The actual name of the mod being used
    pub id: String,
}

impl ModMetadata {
    pub fn deserialize_from_str<S: AsRef<str>, N: Into<String>>(
        source: S,
        name: N,
    ) -> Result<Self> {
        let mut lines = source.as_ref().lines();
        let _header = lines
            .next()
            .filter(|line| line.contains("[mod]"))
            .ok_or(Error::MissingField("header"))?;

        macro_rules! parse_line {
            ($label:expr,$type:ty) => {
                lines
                    .next()
                    .and_then(|line| line.split_once("="))
                    .map(|(left, right)| {
                        if !left.eq_ignore_ascii_case($label) {
                            return Err(Error::MismatchedField($label, left.to_owned()));
                        }
                        right
                            .parse::<$type>()
                            .map_err(|err| Error::ParseError(Box::new(err)))
                    })
                    // .ok_or(Error::MissingField($label))
                    .ok_or(Error::MissingField($label))
                    .flatten()
            };
        }

        let published_file_id = parse_line!("publishedFileId", u32)?;
        let title = parse_line!("Title", String)?;
        let description = parse_line!("Description", String)?;
        let requires_xpack = parse_line!("RequiresXPACK", bool).unwrap_or_default();

        Ok(Self {
            published_file_id,
            title,
            description,
            requires_xpack,
            id: name.into(),
        })
    }
}
