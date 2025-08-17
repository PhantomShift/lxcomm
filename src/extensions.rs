use std::borrow::Cow;

use itertools::Itertools;
use steam_rs;

use crate::{files, xcom_mod};

pub trait DetailsExtension {
    fn get_description(&self) -> &str;
    fn get_score(&self) -> f32;
}

impl DetailsExtension for steam_rs::published_file_service::query_files::File {
    fn get_description(&self) -> &str {
        self.file_description
            .as_ref()
            .or(self.short_description.as_ref())
            .map_or("", |desc| desc.as_str())
    }

    fn get_score(&self) -> f32 {
        self.vote_data.as_ref().map_or(0.0, |data| data.score)
    }
}

impl DetailsExtension for xcom_mod::ModMetadata {
    fn get_description(&self) -> &str {
        &self.description
    }

    fn get_score(&self) -> f32 {
        0.0
    }
}

impl DetailsExtension for files::ModDetails {
    fn get_description(&self) -> &str {
        match self {
            Self::Workshop(details) => details.get_description(),
            Self::Local(details) => details.get_description(),
        }
    }

    fn get_score(&self) -> f32 {
        match self {
            Self::Workshop(details) => details.get_score(),
            Self::Local(details) => details.get_score(),
        }
    }
}

pub trait SplitAtBoundary {
    fn split_at_boundary(&self, predicate: fn(char, char) -> bool) -> impl Iterator<Item = &str>;
    fn split_at_case(&self) -> impl Iterator<Item = &str> {
        self.split_at_boundary(|l, r| l.is_lowercase() && r.is_uppercase())
    }
}

impl<S> SplitAtBoundary for S
where
    S: AsRef<str>,
{
    fn split_at_boundary(&self, predicate: fn(char, char) -> bool) -> impl Iterator<Item = &str> {
        let mut last_index = 0;
        let mut indices = self
            .as_ref()
            .char_indices()
            .tuple_windows()
            .map(move |((_li, lc), (ri, rc))| (ri, predicate(lc, rc)));

        std::iter::from_fn(move || {
            if last_index >= self.as_ref().len() {
                return None;
            }

            for (i, should_split) in indices.by_ref() {
                if should_split {
                    let slice = &self.as_ref()[last_index..i];
                    last_index = i;
                    return Some(slice);
                }
            }
            let remaining = &self.as_ref()[last_index..];
            last_index = self.as_ref().len();
            Some(remaining)
        })
    }
}

// Temporary solution until upstream fix is accepted
// https://github.com/garhow/steam-rs/pull/57
pub trait URLArrayListable: IntoIterator {
    type Item: ToString;
    fn into_url_array(self, name: &str) -> String;
}

impl<T> URLArrayListable for T
where
    T: IntoIterator,
    <T as std::iter::IntoIterator>::Item: ToString,
{
    type Item = T::Item;

    // https://steamcommunity.com/workshop/browse/?appid=268500&searchtext=hololive+test&childpublishedfileid=0&browsesort=trend&section=readytouseitems&requiredtags%5B%5D=War+of+the+Chosen&created_date_range_filter_start=0&created_date_range_filter_end=0&updated_date_range_filter_start=0&updated_date_range_filter_end=0

    fn into_url_array(self, name: &str) -> String {
        let c = self
            .into_iter()
            .enumerate()
            .map(|(i, item)| format!("{name}[{i}]={}", item.to_string()))
            .collect::<Vec<_>>();

        c.join("&")
    }
}

pub trait Truncatable<'a> {
    type Slice: 'a + ?Sized + ToOwned;
    const OVERFLOW: &'a Self::Slice;

    fn truncated(&'a self, max: usize) -> Cow<'a, Self::Slice>;
    fn truncated_overflow(&'a self, max: usize) -> Cow<'a, Self::Slice>;
}

impl<'a, S> Truncatable<'a> for S
where
    S: AsRef<str>,
{
    type Slice = str;
    const OVERFLOW: &'a Self::Slice = "...";

    fn truncated(&'a self, max: usize) -> Cow<'a, str> {
        if self.as_ref().len() > max {
            let mut s = String::from(self.as_ref());
            let mut peek = s.char_indices().peekable();
            while peek.next_if(|(id, _)| *id < max).is_some() {
                peek.next();
            }
            let index = peek.next().map(|(i, _)| i).unwrap_or_default();
            s.truncate(index);
            Cow::Owned(s)
        } else {
            Cow::Borrowed(self.as_ref())
        }
    }

    fn truncated_overflow(&'a self, max: usize) -> Cow<'a, Self::Slice> {
        match self.truncated(max) {
            Cow::Owned(mut s) => {
                s.push_str(Self::OVERFLOW);
                Cow::Owned(s)
            }
            borrowed @ Cow::Borrowed(_) => borrowed,
        }
    }
}
