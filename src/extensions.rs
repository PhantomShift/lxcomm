use steam_rs;

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
            .map_windows(move |[(_li, lc), (ri, rc)]| (*ri, predicate(*lc, *rc)));

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
