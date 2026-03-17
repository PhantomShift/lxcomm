use ego_tree::iter::Edge;

/// A span representing the styles applied to a given section of text
#[derive(Debug)]
pub struct Span {
    pub text: String,
    pub heading: Option<u8>,
    pub link: Option<String>,
    pub underline: bool,
    pub strikethrough: bool,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug)]
pub enum Item {
    Breakline,
    Rule,
    Span(Span),
    Table(Vec<Vec<Item>>),
    OrderedList(Vec<Item>),
    UnorderedList(Vec<Item>),
    Quote {
        author: Option<String>,
        items: Vec<Item>,
    },
    Code(Vec<Item>),
    Group(Vec<Item>),

    // Parsing-only (eventually converted into one of the other items)
    TableRow,
    TableItem(Vec<Item>),
}

impl Item {
    fn push_item(&mut self, value: Self) {
        match self {
            Self::TableItem(items) => items.push(value),
            Self::OrderedList(list) if !matches!(value, Item::Breakline) => list.push(value),
            Self::UnorderedList(list) if !matches!(value, Item::Breakline) => list.push(value),
            Self::Quote { author: _, items } => items.push(value),
            Self::Code(list) => list.push(value),
            _ => (),
        }
    }
}

pub fn process_description(
    description: scraper::ElementRef,
) -> Result<Vec<Item>, crate::error::Error> {
    let mut heading = 0;
    let mut link: Option<&str> = None;
    let mut underline = false;
    let mut strikethrough = false;
    let mut bold = false;
    let mut italic = false;

    let mut items: Vec<Item> = Vec::new();
    let mut item_stack: Vec<Item> = Vec::new();

    macro_rules! push_span {
        ($text:expr) => {{
            let span = Span {
                text: $text.to_string(),
                heading: (heading > 0).then_some(heading),
                link: link.map(|s| s.to_string()),
                underline,
                strikethrough,
                bold,
                italic,
            };
            let span = Item::Span(span);
            if let Some(item) = item_stack.last_mut() {
                item.push_item(span);
            } else {
                items.push(span);
            }
        }};
    }

    macro_rules! push_item {
        ($item:expr) => {{
            if let Some(item) = item_stack.last_mut() {
                item.push_item($item);
            } else {
                items.push($item);
            }
        }};
    }

    for edge in description.traverse() {
        match edge {
            Edge::Open(node) => {
                let val = node.value();
                if let Some(text) = val.as_text() {
                    push_span!(&text);
                } else if let Some(elem) = val.as_element() {
                    match elem.name.local {
                        local_name!("br") => push_item!(Item::Breakline),
                        local_name!("ol") => item_stack.push(Item::OrderedList(Vec::new())),
                        local_name!("ul") => item_stack.push(Item::UnorderedList(Vec::new())),
                        local_name!("b") => bold = true,
                        local_name!("i") => italic = true,
                        local_name!("s") => strikethrough = true,
                        local_name!("u") => underline = false,
                        local_name!("a") => link = elem.attr("href"),
                        local_name!("blockquote") => item_stack.push(Item::Quote {
                            author: None,
                            items: Vec::new(),
                        }),
                        local_name!("span") => {
                            if let Some(class) = elem.classes().next()
                                && class == "bb_strike"
                            {
                                strikethrough = true;
                            }
                        }
                        local_name!("div") => {
                            if let Some(class) = elem.classes().next() {
                                match class {
                                    "bb_code" => item_stack.push(Item::Code(Vec::new())),
                                    "bb_table" => item_stack.push(Item::Table(Vec::new())),
                                    "bb_table_tr" => item_stack.push(Item::TableRow),
                                    "bb_table_th" | "bb_table_td" => {
                                        item_stack.push(Item::TableItem(Vec::new()))
                                    }
                                    "bb_h1" => heading = 1,
                                    "bb_h2" => heading = 2,
                                    "bb_h3" => heading = 3,
                                    _ => (),
                                }
                            }
                        }
                        _ => (),
                    }
                }
            }
            Edge::Close(node) => {
                if let Some(elem) = node.value().as_element() {
                    match elem.name.local {
                        local_name!("ol") => {
                            let ol = item_stack
                                .pop_if(|i| matches!(i, Item::OrderedList(_)))
                                .ok_or("ordered list open should exist")?;
                            push_item!(ol);
                        }
                        local_name!("ul") => {
                            let ul = item_stack
                                .pop_if(|i| matches!(i, Item::UnorderedList(_)))
                                .ok_or("unordered list open should exist")?;
                            push_item!(ul);
                        }
                        local_name!("b") => bold = false,
                        local_name!("i") => italic = false,
                        local_name!("s") => strikethrough = false,
                        local_name!("u") => underline = false,
                        local_name!("a") => link = None,
                        local_name!("blockquote") => {
                            // Not sure if nested quotes exist but they might, so!
                            let quote = item_stack
                                .pop_if(|i| matches!(i, Item::Quote { .. }))
                                .ok_or("blockquote open should exist")?;
                            push_item!(quote);
                        }
                        // Assuming no nested spans
                        local_name!("span") => strikethrough = false,
                        local_name!("div") => {
                            if let Some(class) = elem.classes().next() {
                                match class {
                                    "bb_code" => {
                                        let code = item_stack
                                            .pop_if(|i| matches!(i, Item::Code(_)))
                                            .ok_or("code block open should exist")?;
                                        push_item!(code);
                                    }
                                    "bb_table" => {
                                        let index = item_stack
                                            .iter()
                                            .take_while(|item| !matches!(item, Item::Table(_)))
                                            .count();
                                        let mut t_items = item_stack.drain(index..);
                                        let Item::Table(mut rows) =
                                            t_items.next().ok_or("trailing table tag")?
                                        else {
                                            return Err("failed to get opening table tag")?;
                                        };
                                        let mut curr_row = None;
                                        for item in t_items {
                                            match item {
                                                Item::TableRow => {
                                                    if let Some(row) = curr_row.take() {
                                                        rows.push(row);
                                                    } else {
                                                        curr_row = Some(Vec::new())
                                                    }
                                                }
                                                Item::TableItem(items) => {
                                                    if let Some(row) = curr_row.as_mut() {
                                                        row.push(Item::Group(items));
                                                    } else {
                                                        Err(
                                                            "table item without corresponding table row",
                                                        )?
                                                    }
                                                }
                                                _ => return Err("unexpected item in stack")?,
                                            }
                                        }
                                    }
                                    "bb_h1" | "bb_h2" | "bb_h3" => {
                                        push_item!(Item::Breakline);
                                        heading = 0;
                                    }
                                    _ => (),
                                }
                            }
                        }
                        _ => (),
                    }
                }
            }
        }
    }

    Ok(items)
}

#[tokio::test]
async fn test_process_description() -> Result<(), Box<dyn std::error::Error>> {
    let highlander =
        reqwest::get("https://steamcommunity.com/sharedfiles/filedetails/?id=1134256495&l=english")
            .await?
            .text()
            .await?;
    let doc = scraper::Html::parse_document(&highlander);
    let description = doc
        .select(&crate::selectors::DESCRIPTION)
        .next()
        .ok_or("missing description")?;

    let items = process_description(description)?;
    println!("{items:#?}");

    Ok(())
}
