use std::collections::HashMap;

use bstr::ByteSlice;
use iced::widget::markdown::{self, Item};
use itertools::Itertools;

// Probably(?) important todo: make this not unbounded
#[derive(Debug, Default)]
pub struct MarkupCache {
    cached_results: HashMap<String, Vec<Item>>,
}

impl MarkupCache {
    pub fn cache_markup<S: AsRef<str>>(&mut self, subject: S) {
        if !self.cached_results.contains_key(subject.as_ref()) {
            let subject = subject.as_ref().to_owned();
            let md = to_markdown(&subject);
            let res = markdown::parse(&md).collect();
            self.cached_results.insert(subject, res);
        }
    }

    pub fn get_markup<S: AsRef<str>>(&self, subject: S) -> Option<&[Item]> {
        self.cached_results.get(subject.as_ref()).map(Vec::as_slice)
    }
}

fn find_pair(subject: &str, left: &str, right: &str) -> Option<(usize, usize)> {
    let l = subject.find(left)?;
    let r = subject[l + left.len()..].find(right)?;

    Some((l, l + left.len() + r))
}

// does additional matching at the `+` character
// for simplicity, only on the left pattern, and only once
// panics if there is no `+` in the left pattern
fn match_pair(subject: &str, left: &str, right: &str) -> Option<(usize, usize, usize)> {
    let (left_front, left_back) = left.split_once('+').expect("pattern should contain a '+'");
    let l = subject.find(left_front)?;
    #[allow(clippy::sliced_string_as_bytes)]
    let i = subject[l + left_front.len()..]
        .as_bytes()
        .windows(left_back.len() + 1)
        .enumerate()
        .find_map(|(i, chars)| chars.ends_with_str(left_back).then_some(i + 1))?;
    let r = subject[l + left_front.len() + i + left_back.len()..].find(right)?;

    Some((
        l,
        l + left_front.len() + i + left_back.len(),
        l + left_front.len() + i + left_back.len() + r,
    ))
}

pub fn to_markdown<S: AsRef<str>>(subject: S) -> String {
    let subject = subject.as_ref();
    if let Some((l, r)) = find_pair(subject, "[noparse]", "[/noparse") {
        let front = to_markdown(&subject[..l]);
        let raw = &subject[l + "[noparse]".len()..r];
        let back = to_markdown(&subject[r + "[/noparse]".len()..]);

        return front + raw + &back;
    }

    if let Some((l, r)) = find_pair(subject, "[code]", "[/code]") {
        let front = to_markdown(&subject[..l]);
        let monospace = &subject[l + "[code]".len()..r];
        let start_block = if monospace.starts_with('\n') {
            "```"
        } else {
            "```\n"
        };
        let end_block = if monospace.ends_with('\n') {
            "```"
        } else {
            "\n```"
        };
        let back = to_markdown(&subject[r + "[/code]".len()..]);

        return front + start_block + monospace + end_block + &back;
    }

    if let Some((l, r)) = find_pair(subject, "[hr]", "[/hr]") {
        let front = to_markdown(&subject[..l]);
        let middle = "\n---\n";
        let back = to_markdown(&subject[r + "[/hr]".len()..]);

        return front + middle + &back;
    }

    if let Some((l, i, r)) = match_pair(subject, "[url+]", "[/url]") {
        let front = to_markdown(&subject[..l]);
        let link = if i - l > "[url]".len() {
            &subject[l + "[url=".len()..i - 1]
        } else {
            ""
        };
        let formatted = format!("[{}]({link})", to_markdown(&subject[i..r]));
        let back = to_markdown(&subject[r + "[/url]".len()..]);

        return front + &formatted + &back;
    }

    if let Some((l, r)) = find_pair(subject, "[quote]", "[/quote]") {
        let front = to_markdown(&subject[..l]);
        let quote = subject[l + "[quote]".len()..r]
            .lines()
            .map(|line| format!("> {}", to_markdown(line)))
            .join("\n");
        let back = to_markdown(&subject[r + "[/quote]".len()..]);

        return front + &quote + &back;
    }

    if let Some((l, i, r)) = match_pair(subject, "[quote+]", "[/quote]") {
        let front = to_markdown(&subject[..l]);
        let author = if i - l > "[quote]".len() {
            &subject[l + "[quote=".len()..i - 1]
        } else {
            "Unknown"
        };
        let author_line = format!("> *Originally posted by **{author}**\n");
        let quote = subject[i..r]
            .lines()
            .map(|line| format!("> {}", to_markdown(line)))
            .join("\n");
        let back = to_markdown(&subject[r + "[/quote]".len()..]);

        return front + &author_line + &quote + &back;
    }

    if let Some((l, r)) = find_pair(subject, "[list]", "[/list]") {
        let front = to_markdown(&subject[..l]);
        let items = subject[l + "[list]".len()..r]
            .lines()
            .map(|line| format!("{}\n", to_markdown(line)))
            .collect::<String>();
        let back = to_markdown(&subject[r + "[/list]".len()..]);

        return front + &items + &back;
    }

    if let Some((l, r)) = find_pair(subject, "[olist]", "[/olist]") {
        let front = to_markdown(&subject[..l]);
        let mut index = 0;
        let items = subject[l + "[olist]".len()..r]
            .lines()
            .map(|line| {
                let formatted = to_markdown(line);
                let replaced = formatted.replacen(" - ", &format!(" {}. ", index + 1), 1);
                let formatted = if replaced != formatted {
                    index += 1;
                    replaced
                } else {
                    formatted
                };

                format!("{formatted}\n")
            })
            .collect::<String>();
        let back = to_markdown(&subject[r + "[/olist]".len()..]);

        return front + &items + &back;
    }

    // TODO: Tables

    // only handling single-digit header,
    // presumably more than 3 is useless anyways
    if let Some(l) = subject.find("[h")
        && let Some(ch) = subject
            .chars()
            .nth(l + "[h".len())
            .filter(char::is_ascii_digit)
    {
        let level = ch as usize - '0' as usize;
        let closing_tag = format!("[/h{ch}]");
        if let Some(r) = subject[l + "[hn]".len()..].find(&closing_tag) {
            let r = l + "[hn]".len() + r;
            let front = to_markdown(&subject[..l]);
            let inner = format!(
                "{} {}",
                "#".repeat(level),
                to_markdown(&subject[l + "[hn]".len()..r])
            );
            let back = to_markdown(&subject[r + "[/hn]".len()..]);
            return front + &inner + &back;
        }
    }

    // Just naively convert by substition
    subject
        .replace("[*]", " - ")
        .replace("[b]", "**")
        .replace("[/b]", "**")
        .replace("[u]", "__")
        .replace("[/u]", "__")
        .replace("[i]", "*")
        .replace("[/i]", "*")
        .replace("[strike]", "~~")
        .replace("[/strike]", "~~")
        // Don't know if iced actually handles this
        .replace("[spoiler]", "||")
        .replace("[/spoiler]", "||")
}

#[test]
fn test_unordered() {
    let input = r#"
[list]
    [*]Bulleted list
    [*]Bulleted list
    [*]Bulleted list
[/list] 
    "#;

    println!("{}", to_markdown(input));
}

#[test]
fn test_ordered() {
    let input = r#"
[olist]
    [*]Ordered list
    [*]Ordered list
    [*]Ordered list
[/olist] 
    "#;

    println!("{}", to_markdown(input));
}
