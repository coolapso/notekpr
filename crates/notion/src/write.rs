//! Writing what was read as notes, in two passes: first every page and
//! database gets its place (a note, a folder, or a spot in the page that
//! shows it), then each note is written, so every link between pages finds
//! where its page went.

use std::collections::{BTreeMap, BTreeSet};

use notekpr_core::account_export::unique_name;
use notekpr_core::paths;

use crate::dom::{Element, Node};
use crate::read::{Database, Page, Parent, Prop, View, Workspace, view_of};
use crate::text::{self, escape, file_name};
use crate::{ATTACHMENTS, Converted, DatabaseReport, ID_KEY, Options, REPORT, Report};

/// How a database comes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// A folder with a note per row and an index note with their table: a
    /// full-page database.
    Folder,
    /// Tasks (it has a status or a checkbox): a checklist where it's shown,
    /// or a note of its own for a full-page database whose rows say
    /// nothing more. A row that does gets a note beside the page.
    Checklist,
    /// Inline, and no row says more than its properties: a table where
    /// it's shown.
    Table,
    /// Inline, rows that say more: a table where it's shown, and a folder
    /// beside it with a note per row.
    Notes,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Item {
    Page(String),
    Database(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum What {
    Page,
    Index,
    Checklist,
}

#[derive(Clone, Debug)]
struct Placed {
    what: What,
    /// Its note, if it has one (a page with nothing but subpages is only a
    /// folder).
    note: Option<String>,
    folder: Option<String>,
}

/// What a note is being written from.
struct Cx {
    export: usize,
    /// The folder in the export its links are relative to.
    dir: String,
    /// The note being written.
    note: String,
    /// The page being written, if it's a page.
    page: String,
}

/// A front matter value.
enum Value {
    /// Text: quoted where YAML needs it.
    Str(String),
    /// A number, a date, `true`: as it is.
    Raw(String),
    List(Vec<Value>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListKind {
    Bullet,
    Number,
    Task,
}

/// A block of Markdown; items of one list follow each other without a
/// blank line.
struct Block {
    text: String,
    list: Option<ListKind>,
}

impl Block {
    fn text(text: impl Into<String>) -> Block {
        Block {
            text: text.into(),
            list: None,
        }
    }
}

pub struct Writer<'a> {
    ws: &'a Workspace,
    opts: &'a Options,
    shapes: BTreeMap<String, Shape>,
    /// Pages under each page.
    children: BTreeMap<String, Vec<String>>,
    placed: BTreeMap<String, Placed>,
    /// Rows without a note of their own: the page or database note showing
    /// them.
    shown_in: BTreeMap<String, String>,
    /// Names taken in each folder (lower case).
    taken: BTreeMap<String, BTreeSet<String>>,
    /// How many notes have each name (lower case), here and in the vault.
    stems: BTreeMap<String, usize>,
    /// Names of the vault's other files (lower case).
    vault_files: BTreeSet<String>,
    /// Where each file of the export goes.
    attachments: BTreeMap<(usize, String), String>,
    report: Report,
}

fn is_inline(n: &Node) -> bool {
    match n {
        Node::Text(_) => true,
        Node::Element(e) => matches!(
            e.name.as_str(),
            "a" | "abbr"
                | "b"
                | "br"
                | "code"
                | "del"
                | "em"
                | "i"
                | "img"
                | "input"
                | "kbd"
                | "label"
                | "mark"
                | "s"
                | "small"
                | "span"
                | "strong"
                | "sub"
                | "sup"
                | "time"
                | "u"
        ),
    }
}

/// Blocks as Markdown: a blank line between them, none between items of
/// one list.
fn join(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            let family = |k: Option<ListKind>| k.map(|k| k == ListKind::Number);
            let tight = b.list.is_some() && family(b.list) == family(blocks[i - 1].list);
            out.push_str(if tight { "\n" } else { "\n\n" });
        }
        out.push_str(&b.text);
    }
    out
}

/// Every line but blank ones `n` spaces in.
fn indent(text: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    text.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every line after the first `n` spaces in.
fn indent_rest(text: &str, n: usize) -> String {
    match text.split_once('\n') {
        Some((first, rest)) => format!("{first}\n{}", indent(rest, n)),
        None => text.to_string(),
    }
}

/// Every line after `> `: a quote.
fn quoted(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                ">".to_string()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Inline Markdown as a paragraph: lines without the spaces around them,
/// nothing at their start that Markdown would read as a block, no more
/// than one blank line in a row.
fn paragraph(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() && lines.last().is_none_or(|p| p.is_empty()) {
            continue;
        }
        lines.push(text::escape_line_start(l));
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// `s` inside `mark` on both sides, the spaces around it outside them
/// (`** a**` doesn't make bold text).
fn wrap(mark: &str, s: &str) -> String {
    let core = s.trim();
    if core.is_empty() {
        return s.to_string();
    }
    let lead = &s[..s.len() - s.trim_start().len()];
    let trail = &s[s.trim_end().len()..];
    format!("{lead}{mark}{core}{mark}{trail}")
}

/// The palette's colors as the toolbar writes them
/// (`crates/editor/src/color.rs`), for Notion's color names.
fn palette(name: &str, background: bool) -> Option<&'static str> {
    let i = match name {
        "gray" => 0,
        "red" => 1,
        "orange" | "brown" => 2,
        "yellow" => 3,
        "green" | "teal" => 4,
        "blue" => 5,
        "purple" => 6,
        "pink" => 7,
        _ => return None,
    };
    const TEXT: [&str; 8] = [
        "#7c7c7c", "#e44045", "#c46114", "#9c7705", "#298e5d", "#3a7bdf", "#9664d6", "#d6419f",
    ];
    const BACKGROUND: [&str; 8] = [
        "#8c8c8c4d",
        "#e5484d4d",
        "#f7a2404d",
        "#ffd0004d",
        "#30a46c4d",
        "#3e7ee04d",
        "#9a6bd84d",
        "#d6409f4d",
    ];
    Some(if background { BACKGROUND[i] } else { TEXT[i] })
}

/// The color an element gives its text in Notion (`block-color-red`,
/// `highlight-blue_background`, …): its name, and whether it's a
/// background.
fn notion_color(e: &Element) -> Option<(String, bool)> {
    let c = e
        .classes()
        .find_map(|c| {
            c.strip_prefix("block-color-")
                .or_else(|| c.strip_prefix("highlight-"))
        })
        .or_else(|| e.attr("data-notion-highlight"))?;
    match c.strip_suffix("_background") {
        Some(name) => Some((name.to_string(), true)),
        None => Some((c.to_string(), false)),
    }
}

/// `s` in the color `e` gives it.
fn colored(e: &Element, s: String) -> String {
    let Some((name, background)) = notion_color(e) else {
        return s;
    };
    let Some(css) = palette(&name, background) else {
        return s;
    };
    if s.trim().is_empty() {
        return s;
    }
    let prop = if background {
        "background-color"
    } else {
        "color"
    };
    let open = format!("<span style=\"{prop}: {css}\">");
    if s.starts_with(&open) && s.ends_with("</span>") && s.matches("<span").count() == 1 {
        return s;
    }
    format!("{open}{s}</span>")
}

/// A callout's type, by its color.
fn callout_type(e: &Element) -> &'static str {
    match notion_color(e).map(|(n, _)| n).as_deref() {
        Some("red") => "danger",
        Some("orange" | "yellow" | "brown") => "warning",
        Some("green" | "teal") => "success",
        Some("blue") => "info",
        Some("purple") => "abstract",
        _ => "note",
    }
}

/// Notion's name for a code block's language as notekpr knows it.
fn language(notion: &str) -> String {
    let l = notion.trim().to_lowercase();
    match l.as_str() {
        "plain text" | "plaintext" | "text" | "none" => String::new(),
        "c++" => "cpp".into(),
        "c#" => "csharp".into(),
        "f#" => "fsharp".into(),
        "shell" | "sh" => "bash".into(),
        "objective-c" => "objc".into(),
        "vb.net" => "vbnet".into(),
        _ => l.replace(' ', "-"),
    }
}

fn escaped(s: &str) -> String {
    let mut out = String::new();
    escape(s, &mut out);
    out
}

fn html_escaped(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A table cell's Markdown: on one line, its `|` escaped, wiki links
/// without their alias (notekpr reads `[[a\\|b]]` as a link to `a\\`).
fn cell_text(s: &str) -> String {
    let mut plain = String::new();
    let mut rest = s.trim();
    while let Some(open) = rest.find("[[") {
        let Some(close) = rest[open..].find("]]") else {
            break;
        };
        let inner = &rest[open + 2..open + close];
        plain.push_str(&rest[..open + 2]);
        plain.push_str(inner.split('|').next().unwrap_or(inner));
        plain.push_str("]]");
        rest = &rest[open + close + 2..];
    }
    plain.push_str(rest);
    plain
        .replace('|', "\\|")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("<br>")
}

/// A link's text without the page icon Notion puts in it.
fn label(e: &Element) -> String {
    let mut out = String::new();
    for n in &e.children {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Element(c) if c.has_class("icon") => {}
            Node::Element(c) if c.name == "br" => out.push(' '),
            Node::Element(c) => out.push_str(&label(c)),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The options chosen in a select or multi-select cell.
fn selected(cell: &Element) -> Vec<String> {
    let mut spans = Vec::new();
    cell.find_all(&|e| e.has_class("selected-value"), &mut spans);
    spans
        .iter()
        .map(|s| s.text().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn status(cell: &Element) -> Option<(String, Option<String>)> {
    let s = cell.find(&|e| e.has_class("status-value"))?;
    let text = s.text().trim().to_string();
    let color = s
        .classes()
        .find_map(|c| c.strip_prefix("select-value-color-"))
        .map(str::to_string);
    (!text.is_empty()).then_some((text, color))
}

fn times(cell: &Element) -> Vec<String> {
    let mut found = Vec::new();
    cell.find_all(
        &|e| e.name == "time" && e.attr("datetime").is_some(),
        &mut found,
    );
    found
        .iter()
        .filter_map(|t| t.attr("datetime"))
        .map(str::to_string)
        .collect()
}

fn checked(cell: &Element) -> bool {
    cell.find(&|e| e.has_class("checkbox-on") || e.attr("checked").is_some())
        .is_some()
}

/// `rel` (a path in a link, decoded) from the folder `dir`, `..` and all.
fn resolve(dir: &str, rel: &str) -> Option<String> {
    let mut segs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for s in rel.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop()?;
            }
            s => segs.push(s),
        }
    }
    Some(segs.join("/"))
}

/// Whether a link goes somewhere outside the export.
fn external(href: &str) -> bool {
    href.contains("://") || href.starts_with("mailto:") || href.starts_with("data:")
}

fn notion_url(href: &str) -> bool {
    href.contains("notion.so") || href.contains("notion.com") || href.contains("notion.site")
}

fn front_matter(fields: &[(String, Value)]) -> String {
    let one = |v: &Value| match v {
        Value::Str(s) => text::yaml(s),
        Value::Raw(s) => s.clone(),
        Value::List(_) => String::new(),
    };
    let mut out = String::from("---\n");
    for (k, v) in fields {
        let k = text::yaml(k);
        match v {
            Value::List(items) => {
                out.push_str(&format!("{k}:\n"));
                for i in items {
                    out.push_str(&format!("  - {}\n", one(i)));
                }
            }
            v => out.push_str(&format!("{k}: {}\n", one(v))),
        }
    }
    out.push_str("---\n");
    out
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl<'a> Writer<'a> {
    pub fn new(ws: &'a Workspace, opts: &'a Options) -> Writer<'a> {
        let mut w = Writer {
            ws,
            opts,
            shapes: BTreeMap::new(),
            children: BTreeMap::new(),
            placed: BTreeMap::new(),
            shown_in: BTreeMap::new(),
            taken: BTreeMap::new(),
            stems: BTreeMap::new(),
            vault_files: BTreeSet::new(),
            attachments: BTreeMap::new(),
            report: Report::default(),
        };
        for (child, parent) in &ws.parents {
            if let Parent::Page(p) = parent {
                w.children.entry(p.clone()).or_default().push(child.clone());
            }
        }
        for db in ws.databases.values() {
            w.shapes.insert(db.id.clone(), w.shape_of(db));
        }
        w
    }

    fn shape_of(&self, db: &Database) -> Shape {
        let rows = db.rows.iter().filter_map(|r| self.ws.pages.get(r));
        let tasks = rows.clone().any(|p| {
            p.props
                .iter()
                .any(|p| p.kind == "status" || p.kind == "checkbox")
        });
        let content = rows.clone().any(Page::has_content);
        let inline = db.host.is_some();
        if tasks && (inline || !content) {
            Shape::Checklist
        } else if !inline {
            Shape::Folder
        } else if content {
            Shape::Notes
        } else {
            Shape::Table
        }
    }

    /// The database most of a view's rows are rows of.
    fn source_of(&self, view: &View) -> Option<String> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for r in &view.rows {
            if let Some(db) = self.ws.database_of_row(r) {
                *counts.entry(&db.id).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|&(id, n)| (n, std::cmp::Reverse(id)))
            .map(|(id, _)| id.to_string())
    }

    pub fn run(mut self) -> Converted {
        self.layout();
        self.write()
    }

    // ------------------------------------------------------------ layout

    fn layout(&mut self) {
        let ws = self.ws;
        let root = self.opts.into.clone();
        let taken = self.taken.entry(root.clone()).or_default();
        taken.insert(paths::title(REPORT).to_lowercase());
        taken.insert(ATTACHMENTS.to_lowercase());
        let mut top = Vec::new();
        for p in ws.pages.values() {
            if !ws.parents.contains_key(&p.id) && !ws.templates.contains_key(&p.id) {
                top.push(Item::Page(p.id.clone()));
            }
        }
        for db in ws.databases.values() {
            if db.page.is_none() && db.host.is_none() {
                top.push(Item::Database(db.id.clone()));
            }
        }
        for item in self.sorted(top) {
            self.place(&item, &root);
        }
        // Rows shown in a page, without a note of their own.
        for db in ws.databases.values() {
            let Some(host) = db.host.as_ref() else {
                continue;
            };
            for r in &db.rows {
                if !self.placed.contains_key(r) {
                    self.shown_in.insert(r.clone(), host.clone());
                }
            }
        }
        for (id, p) in &self.placed {
            if let Some(note) = &p.note
                && !self.opts.existing.contains_key(id)
            {
                *self
                    .stems
                    .entry(paths::title(note).to_lowercase())
                    .or_default() += 1;
            }
        }
        for path in &self.opts.vault_paths {
            if paths::is_note(path) {
                *self
                    .stems
                    .entry(paths::title(path).to_lowercase())
                    .or_default() += 1;
            } else {
                self.vault_files
                    .insert(paths::file_name(path).to_lowercase());
            }
        }
    }

    fn item_name(&self, item: &Item) -> String {
        match item {
            Item::Page(id) => match self.ws.database_of_page(id) {
                Some(db) => file_name(&db.title),
                None => file_name(&self.ws.pages[id].title_iso),
            },
            Item::Database(id) => file_name(&self.ws.databases[id].title),
        }
    }

    /// Items in the order their names are given out: by name, then ID.
    fn sorted(&self, mut items: Vec<Item>) -> Vec<Item> {
        items.sort_by_cached_key(|i| (self.item_name(i).to_lowercase(), i.clone()));
        items
    }

    /// `base`, or `base 2`, … whichever isn't taken in `folder` yet.
    fn unique_in(&mut self, folder: &str, base: &str) -> String {
        let taken = self.taken.entry(folder.to_string()).or_default();
        let name = unique_name(base, |n| taken.contains(&n.to_lowercase()));
        taken.insert(name.to_lowercase());
        name
    }

    fn place(&mut self, item: &Item, folder: &str) {
        match item {
            Item::Page(id) => self.place_page(id, folder),
            Item::Database(id) => self.place_database(id, folder),
        }
    }

    /// What goes inside a page's folder: its subpages, and rows of the
    /// databases it shows that get notes.
    fn kids(&self, id: &str) -> Vec<Item> {
        let ws = self.ws;
        let mut kids: Vec<Item> = self
            .children
            .get(id)
            .into_iter()
            .flatten()
            .filter(|c| !ws.templates.contains_key(*c))
            .map(|c| Item::Page(c.clone()))
            .collect();
        for db in ws.databases.values() {
            if db.host.as_deref() != Some(id) || self.opts.skip.contains(&db.id) {
                continue;
            }
            match self.shapes[&db.id] {
                Shape::Notes => kids.push(Item::Database(db.id.clone())),
                Shape::Checklist => kids.extend(
                    db.rows
                        .iter()
                        .filter(|r| ws.pages.get(*r).is_some_and(Page::has_content))
                        .map(|r| Item::Page(r.clone())),
                ),
                Shape::Folder | Shape::Table => {}
            }
        }
        kids
    }

    fn place_page(&mut self, id: &str, folder: &str) {
        let ws = self.ws;
        if self.opts.skip.contains(id) {
            return;
        }
        if let Some(db) = ws.database_of_page(id) {
            return self.place_database(&db.id.clone(), folder);
        }
        let page = &ws.pages[id];
        let kids = self.kids(id);
        let name = self.unique_in(folder, &file_name(&page.title_iso));
        if kids.is_empty() {
            let note = paths::join(folder, &format!("{name}.md"));
            self.placed.insert(
                id.to_string(),
                Placed {
                    what: What::Page,
                    note: Some(note),
                    folder: None,
                },
            );
            return;
        }
        let sub = paths::join(folder, &name);
        let own = self.says_more(page, &kids);
        if own {
            self.taken
                .entry(sub.clone())
                .or_default()
                .insert(name.to_lowercase());
        }
        self.placed.insert(
            id.to_string(),
            Placed {
                what: What::Page,
                note: own.then(|| paths::join(&sub, &format!("{name}.md"))),
                folder: Some(sub.clone()),
            },
        );
        for kid in self.sorted(kids) {
            self.place(&kid, &sub);
        }
    }

    fn place_database(&mut self, id: &str, folder: &str) {
        let ws = self.ws;
        if self.opts.skip.contains(id) {
            return;
        }
        let db = &ws.databases[id];
        let shape = self.shapes[id];
        if shape == Shape::Table || (shape == Shape::Checklist && db.host.is_some()) {
            return;
        }
        let name = self.unique_in(folder, &file_name(&db.title));
        match shape {
            Shape::Checklist => {
                self.placed.insert(
                    id.to_string(),
                    Placed {
                        what: What::Checklist,
                        note: Some(paths::join(folder, &format!("{name}.md"))),
                        folder: None,
                    },
                );
                for r in &db.rows {
                    self.shown_in.insert(r.clone(), id.to_string());
                }
            }
            Shape::Folder | Shape::Notes => {
                let sub = paths::join(folder, &name);
                let index = shape == Shape::Folder;
                if index {
                    self.taken
                        .entry(sub.clone())
                        .or_default()
                        .insert(name.to_lowercase());
                }
                self.placed.insert(
                    id.to_string(),
                    Placed {
                        what: What::Index,
                        note: index.then(|| paths::join(&sub, &format!("{name}.md"))),
                        folder: Some(sub.clone()),
                    },
                );
                let rows: Vec<Item> = db.rows.iter().map(|r| Item::Page(r.clone())).collect();
                for r in self.sorted(rows) {
                    self.place(&r, &sub);
                }
            }
            Shape::Table => {}
        }
    }

    /// Whether a page with things inside it says more than links to them:
    /// it then gets a note in its folder.
    fn says_more(&self, page: &Page, kids: &[Item]) -> bool {
        if !page.props.is_empty()
            || page.description.is_some()
            || self.ws.database_of_row(&page.id).is_some()
        {
            return true;
        }
        let ids: BTreeSet<&str> = kids
            .iter()
            .filter_map(|k| match k {
                Item::Page(id) => Some(id.as_str()),
                Item::Database(_) => None,
            })
            .collect();
        fn more(e: &Element, ids: &BTreeSet<&str>) -> bool {
            e.children.iter().any(|n| match n {
                Node::Text(t) => !t.trim().is_empty(),
                Node::Element(c) => match c.name.as_str() {
                    "script" | "link" | "style" | "meta" | "br" => false,
                    "p" | "div" | "span" => more(c, ids),
                    "figure" if c.has_class("link-to-page") => !c
                        .find(&|e| e.attr("data-notion-page-id").is_some())
                        .and_then(|a| a.attr("data-notion-page-id"))
                        .and_then(text::notion_id)
                        .is_some_and(|id| ids.contains(id.as_str())),
                    _ => true,
                },
            })
        }
        more(&page.body, &ids)
    }

    // ------------------------------------------------------------- write

    fn write(mut self) -> Converted {
        let ws = self.ws;
        let mut files = Vec::new();
        let mut icons = BTreeMap::new();
        let mut placed: Vec<(String, Placed)> = self
            .placed
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        placed.sort_by(|a, b| (&a.1.note, &a.1.folder).cmp(&(&b.1.note, &b.1.folder)));
        for (id, p) in placed {
            if self.opts.existing.contains_key(&id) {
                self.report.already += 1;
                continue;
            }
            let page = ws.pages.get(&id).or_else(|| {
                ws.databases
                    .get(&id)
                    .and_then(|d| ws.pages.get(d.page.as_ref()?))
            });
            let icon = page.and_then(|p| p.icon.clone());
            if let (Some(folder), Some(icon)) = (&p.folder, &icon) {
                icons.insert(folder.clone(), icon.clone());
            }
            let Some(note) = p.note else {
                continue;
            };
            let text = match p.what {
                What::Page => self.page_note(&id, &note),
                What::Index => self.index_note(&id, &note),
                What::Checklist => self.checklist_note(&id, &note),
            };
            if let Some(icon) = icon {
                icons.insert(note.clone(), icon);
            }
            files.push((note, text.into_bytes()));
        }
        self.report.notes = files.len();
        // Files no note shows come along with the page whose folder they're
        // in, when it's imported now.
        let keys: Vec<(usize, String)> = ws.files.keys().cloned().collect();
        for k in keys {
            if self.attachments.contains_key(&k) {
                continue;
            }
            let mut dir = paths::parent(&k.1);
            let owner = loop {
                if let Some(p) = ws
                    .pages
                    .values()
                    .find(|p| p.export == k.0 && p.folder == dir)
                {
                    break Some(p.id.clone());
                }
                if dir.is_empty() {
                    break None;
                }
                dir = paths::parent(dir);
            };
            let imported =
                |id: &str| !self.opts.existing.contains_key(id) && self.placed.contains_key(id);
            if owner.as_deref().is_none_or(imported) {
                self.place_file(k);
            }
        }
        for (key, path) in &self.attachments {
            files.push((path.clone(), ws.files[key].clone()));
        }
        self.report.files = self.attachments.len();
        self.count_databases();
        for u in &ws.unreadable {
            if let crate::read::Unreadable::Zip(path) = u {
                self.report
                    .lost
                    .entry("Zips inside the export (unzip them and import what's inside)".into())
                    .or_default()
                    .insert(path.clone());
            }
        }
        let report = self.report_note();
        files.push((paths::join(&self.opts.into, REPORT), report.into_bytes()));
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Converted {
            files,
            icons,
            report: self.report,
        }
    }

    fn lost(&mut self, what: &str, cx: &Cx) {
        self.report
            .lost
            .entry(what.to_string())
            .or_default()
            .insert(cx.note.clone());
    }

    fn page_cx(&self, page: &Page, note: &str) -> Cx {
        Cx {
            export: page.export,
            dir: paths::parent(&page.path).to_string(),
            note: note.to_string(),
            page: page.id.clone(),
        }
    }

    fn page_note(&mut self, id: &str, note: &str) -> String {
        let ws = self.ws;
        let page = &ws.pages[id];
        let cx = self.page_cx(page, note);
        let mut fields = Vec::new();
        if let Some(db) = ws.database_of_row(id) {
            let mut used: BTreeSet<String> = BTreeSet::from([ID_KEY.to_string()]);
            for prop in ordered(page, db) {
                let Some(mut v) = self.value(prop, &cx) else {
                    continue;
                };
                let mut key = prop.name.clone();
                if prop.kind == "multi_select" && key.eq_ignore_ascii_case("tags") {
                    key = "tags".into();
                    if let Value::List(items) = v {
                        v = Value::List(
                            items
                                .into_iter()
                                .filter_map(|i| match i {
                                    Value::Str(s) => text::tag(&s).map(Value::Raw),
                                    other => Some(other),
                                })
                                .collect(),
                        );
                    }
                }
                let key = unique_name(&key, |k| used.contains(&k.to_lowercase()));
                used.insert(key.to_lowercase());
                fields.push((key, v));
            }
        }
        fields.push((ID_KEY.into(), Value::Raw(id.to_string())));
        if page.image_icon {
            self.lost("Icons that are pictures", &cx);
        }
        if page.cover {
            self.lost("Covers", &cx);
        }
        let mut blocks = Vec::new();
        if let Some(d) = &page.description {
            let t = paragraph(&self.inline(&d.children, &cx));
            if !t.is_empty() {
                blocks.push(Block::text(t));
            }
        }
        self.flow(&page.body.children, &cx, &mut blocks);
        self.note_text(&fields, &blocks)
    }

    fn note_text(&self, fields: &[(String, Value)], blocks: &[Block]) -> String {
        let mut out = front_matter(fields);
        let body = join(blocks);
        if !body.is_empty() {
            out.push_str(&body);
            out.push('\n');
        }
        out
    }

    /// What a database's notes are written from: its page, or the folder
    /// of its rows.
    fn database_cx(&self, db: &Database, note: &str) -> Cx {
        match db.page.as_ref().and_then(|p| self.ws.pages.get(p)) {
            Some(page) => self.page_cx(page, note),
            None => Cx {
                export: db.export,
                dir: paths::parent(&db.rows_folder).to_string(),
                note: note.to_string(),
                page: String::new(),
            },
        }
    }

    fn database_intro(&mut self, db: &Database, cx: &Cx, blocks: &mut Vec<Block>) {
        let ws = self.ws;
        if let Some(page) = db.page.as_ref().and_then(|p| ws.pages.get(p)) {
            if let Some(d) = &page.description {
                let t = paragraph(&self.inline(&d.children, cx));
                if !t.is_empty() {
                    blocks.push(Block::text(t));
                }
            }
            if page.image_icon {
                self.lost("Icons that are pictures", cx);
            }
            if page.cover {
                self.lost("Covers", cx);
            }
        }
        if !db.templates.is_empty() {
            let names: Vec<String> = db
                .templates
                .iter()
                .filter_map(|t| ws.pages.get(t))
                .map(|p| p.title.clone())
                .collect();
            self.report
                .templates
                .push(format!("{}: {}", db.title, names.join(", ")));
        }
    }

    fn index_note(&mut self, id: &str, note: &str) -> String {
        let db = &self.ws.databases[id];
        let cx = self.database_cx(db, note);
        let mut blocks = Vec::new();
        self.database_intro(db, &cx, &mut blocks);
        let table = self.table(db, true, &cx);
        blocks.push(table);
        self.note_text(&[(ID_KEY.into(), Value::Raw(id.to_string()))], &blocks)
    }

    fn checklist_note(&mut self, id: &str, note: &str) -> String {
        let db = &self.ws.databases[id];
        let cx = self.database_cx(db, note);
        let mut blocks = Vec::new();
        self.database_intro(db, &cx, &mut blocks);
        if let Some(list) = self.checklist(db, &cx) {
            blocks.push(list);
        }
        self.note_text(&[(ID_KEY.into(), Value::Raw(id.to_string()))], &blocks)
    }

    fn count_databases(&mut self) {
        let ws = self.ws;
        let mut inline: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for db in ws.databases.values() {
            if self.opts.skip.contains(&db.id) {
                continue;
            }
            match self.placed.get(&db.id) {
                Some(p) => {
                    let note = p
                        .note
                        .clone()
                        .or_else(|| db.host.as_ref().and_then(|h| self.note_of(h)));
                    self.report.databases.push(DatabaseReport {
                        title: db.title.clone(),
                        note,
                        databases: 1,
                        rows: db.rows.len(),
                    });
                }
                None if db.host.is_some() => {
                    let e = inline.entry(db.title.clone()).or_default();
                    e.0 += 1;
                    e.1 += db.rows.len();
                }
                None => {}
            }
        }
        for (title, (n, rows)) in inline {
            self.report.databases.push(DatabaseReport {
                title,
                note: None,
                databases: n,
                rows,
            });
        }
    }

    fn note_of(&self, id: &str) -> Option<String> {
        self.opts
            .existing
            .get(id)
            .cloned()
            .or_else(|| self.placed.get(id).and_then(|p| p.note.clone()))
    }

    fn report_note(&mut self) -> String {
        let r = self.report.clone();
        let mut out = format!(
            "{} and {} from Notion.",
            count(r.notes, "note", "notes"),
            count(r.files, "file", "files")
        );
        if r.already > 0 {
            out.push_str(&format!(
                " {} {} already in the vault, from an earlier import, and {} imported again.",
                count(r.already, "page", "pages"),
                if r.already == 1 { "was" } else { "were" },
                if r.already == 1 { "wasn't" } else { "weren't" }
            ));
        }
        out.push('\n');
        if !r.databases.is_empty() {
            out.push_str("\n## Databases\n\n| Database | Rows |\n|---|---|\n");
            for d in &r.databases {
                let name = match &d.note {
                    Some(note) => self.wiki(note, &d.title),
                    None if d.databases > 1 => {
                        format!("{}, in {} pages", escaped(&d.title), d.databases)
                    }
                    None => escaped(&d.title),
                };
                out.push_str(&format!("| {} | {} |\n", cell_text(&name), d.rows));
            }
        }
        if !r.waiting.is_empty() {
            out.push_str(
                "\n## Pages to import\n\nLinked from these notes, but not in this export. \
                 The links work once they're imported too.\n\n",
            );
            for (name, from) in &r.waiting {
                let from: Vec<String> = from.iter().map(|n| self.wiki(n, "")).collect();
                out.push_str(&format!("- {}: {}\n", escaped(name), short_list(&from)));
            }
        }
        if !r.lost.is_empty() || !r.templates.is_empty() {
            out.push_str("\n## What didn't come along\n\n");
            for (what, wher) in &r.lost {
                let at: Vec<String> = wher
                    .iter()
                    .map(|n| {
                        if paths::is_note(n) {
                            self.wiki(n, "")
                        } else {
                            escaped(n)
                        }
                    })
                    .collect();
                out.push_str(&format!("- {}: {}\n", escaped(what), short_list(&at)));
            }
            for t in &r.templates {
                out.push_str(&format!(
                    "- Database templates, not imported: {}\n",
                    escaped(t)
                ));
            }
        }
        out
    }

    // ------------------------------------------------------------- links

    /// How to link to the note at `path`: its name, or its path where
    /// another note has that name.
    fn target_for(&self, path: &str) -> String {
        if paths::is_note(path) {
            let stem = paths::title(path);
            if self.stems.get(&stem.to_lowercase()).copied().unwrap_or(0) <= 1 {
                stem.to_string()
            } else {
                path[..path.len() - 3].to_string()
            }
        } else {
            let name = paths::file_name(path);
            if self.vault_files.contains(&name.to_lowercase()) {
                path.to_string()
            } else {
                name.to_string()
            }
        }
    }

    /// A wiki link to `path`, showing `text` where that isn't its name.
    fn wiki(&self, path: &str, text: &str) -> String {
        let target = self.target_for(path);
        alias(&target, paths::title(path), text)
    }

    /// A link to the page or database `id`, its text `text` as Notion
    /// showed it.
    fn link_to(&mut self, id: &str, text: &str, cx: &Cx) -> String {
        let ws = self.ws;
        let text = text.trim();
        let shown = self.shown_in.get(id).and_then(|h| self.note_of(h));
        if let Some(path) = self.note_of(id).or(shown) {
            return self.wiki(&path, text);
        }
        if ws.pages.contains_key(id) || ws.databases.contains_key(id) {
            // In the export, but without a note: a template, a page that
            // is only a folder, one left out.
            return escaped(text);
        }
        // Not in this export: the name it will have once it's imported.
        let name = match text::english_date(text) {
            Some(d) => d.to_string(),
            None => file_name(text),
        };
        self.report
            .waiting
            .entry(name.clone())
            .or_default()
            .insert(cx.note.clone());
        alias(&name, &name, text)
    }

    /// The page a link in the export goes to, by its address.
    fn page_of_href(&self, cx: &Cx, href: &str) -> Option<String> {
        if external(href) {
            return notion_url(href).then(|| text::notion_id(href)).flatten();
        }
        let rel = text::percent_decode(href.split(['?', '#']).next().unwrap_or(""));
        if !rel.to_lowercase().ends_with(".html") {
            return None;
        }
        let path = resolve(&cx.dir, &rel)?;
        self.ws
            .by_path
            .get(&(cx.export, path))
            .cloned()
            .or_else(|| text::notion_id(rel.trim_end_matches(".html")))
    }

    /// Where a file a page links to goes in the vault.
    fn file_target(&mut self, cx: &Cx, href: &str) -> Option<String> {
        if external(href) || href.starts_with('#') {
            return None;
        }
        let rel = text::percent_decode(href.split(['?', '#']).next().unwrap_or(""));
        let key = (cx.export, resolve(&cx.dir, &rel)?);
        self.ws
            .files
            .contains_key(&key)
            .then(|| self.place_file(key))
    }

    fn place_file(&mut self, key: (usize, String)) -> String {
        if let Some(p) = self.attachments.get(&key) {
            return p.clone();
        }
        let name = paths::file_name(&key.1);
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, ""),
        };
        let stem = file_name(stem);
        let folder = paths::join(&self.opts.into, ATTACHMENTS);
        let taken = self.taken.entry(folder.clone()).or_default();
        let stem = unique_name(&stem, |s| {
            taken.contains(&format!("{s}{ext}").to_lowercase())
        });
        let file = format!("{stem}{ext}");
        taken.insert(file.to_lowercase());
        let path = paths::join(&folder, &file);
        self.attachments.insert(key, path.clone());
        path
    }

    // ------------------------------------------------------------ blocks

    /// Nodes as blocks: a run of inline ones as a paragraph, each block
    /// element as its block.
    fn flow(&mut self, nodes: &[Node], cx: &Cx, out: &mut Vec<Block>) {
        let mut run: Vec<Node> = Vec::new();
        for n in nodes {
            if is_inline(n) {
                run.push(n.clone());
                continue;
            }
            self.flush(&mut run, cx, out);
            if let Node::Element(e) = n {
                self.block(e, cx, out);
            }
        }
        self.flush(&mut run, cx, out);
    }

    fn flush(&mut self, run: &mut Vec<Node>, cx: &Cx, out: &mut Vec<Block>) {
        if run.is_empty() {
            return;
        }
        let t = paragraph(&self.inline(run, cx));
        run.clear();
        if !t.is_empty() {
            out.push(Block::text(t));
        }
    }

    fn sub_blocks(&mut self, nodes: &[Node], cx: &Cx) -> String {
        let mut blocks = Vec::new();
        self.flow(nodes, cx, &mut blocks);
        join(&blocks)
    }

    fn block(&mut self, e: &Element, cx: &Cx, out: &mut Vec<Block>) {
        match e.name.as_str() {
            "p" => {
                let t = paragraph(&self.inline(&e.children, cx));
                if !t.is_empty() {
                    out.push(Block::text(colored(e, t)));
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = (e.name.as_bytes()[1] - b'0') as usize;
                let t = self.inline(&e.children, cx);
                let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                if !t.is_empty() {
                    out.push(Block::text(format!(
                        "{} {}",
                        "#".repeat(level),
                        colored(e, t)
                    )));
                }
            }
            "ul" | "ol" => self.list(e, cx, out),
            "blockquote" => {
                let inner = self.sub_blocks(&e.children, cx);
                if !inner.is_empty() {
                    out.push(Block::text(quoted(&inner)));
                }
            }
            "figure" => self.figure(e, cx, out),
            "pre" => out.push(Block::text(code_block(e))),
            "hr" => out.push(Block::text("---")),
            "table" => {
                if let Some(t) = self.simple_table(e, cx) {
                    out.push(Block::text(t));
                }
            }
            "details" => out.push(self.toggle(e, cx)),
            "div" if e.has_class("collection-content") => self.collection(e, cx, out),
            // A database's page: its index note shows the table.
            "div" if e.has_class("collection-content-wrapper") => {}
            "div" if e.has_class("column-list") => {
                self.lost("Columns, shown one after the other", cx);
                for col in e.elements() {
                    self.flow(&col.children, cx, out);
                }
            }
            "nav" => {
                if e.has_class("table_of_contents") {
                    self.lost("Tables of contents", cx);
                }
            }
            "script" | "link" | "style" | "meta" | "input" | "colgroup" => {}
            "video" | "audio" | "source" => {
                if let Some(src) = e.attr("src").or_else(|| {
                    e.find(&|c| c.attr("src").is_some())
                        .and_then(|c| c.attr("src"))
                }) {
                    let src = src.to_string();
                    out.push(Block::text(self.embed(&src, "", cx)));
                }
            }
            _ => self.flow(&e.children, cx, out),
        }
    }

    fn list(&mut self, list: &Element, cx: &Cx, out: &mut Vec<Block>) {
        let toggle = list.has_class("toggle");
        let kind = if list.has_class("to-do-list") {
            ListKind::Task
        } else if list.name == "ol" {
            ListKind::Number
        } else {
            ListKind::Bullet
        };
        let mut n: usize = list.attr("start").and_then(|s| s.parse().ok()).unwrap_or(1);
        for li in list.elements().filter(|e| e.name == "li") {
            if toggle {
                match li.elements().find(|e| e.name == "details") {
                    Some(d) => out.push(self.toggle(d, cx)),
                    None => self.flow(&li.children, cx, out),
                }
                continue;
            }
            let split = li
                .children
                .iter()
                .position(|n| !is_inline(n))
                .unwrap_or(li.children.len());
            let (head, rest) = li.children.split_at(split);
            let done = li.elements().any(|e| {
                e.name == "input" && (e.has_class("checkbox-on") || e.attr("checked").is_some())
            });
            let t = colored(list, paragraph(&self.inline(head, cx)));
            let mut kids = Vec::new();
            self.flow(rest, cx, &mut kids);
            if t.is_empty() && kids.is_empty() && kind != ListKind::Task {
                continue;
            }
            let marker = match kind {
                ListKind::Bullet => "- ".to_string(),
                ListKind::Number => format!("{n}. "),
                ListKind::Task => format!("- [{}] ", if done { 'x' } else { ' ' }),
            };
            n += 1;
            let width = if kind == ListKind::Number {
                marker.len()
            } else {
                2
            };
            let mut item = format!("{marker}{}", indent_rest(&t, width));
            if !kids.is_empty() {
                item.push_str(if kids[0].list.is_some() { "\n" } else { "\n\n" });
                item.push_str(&indent(&join(&kids), width));
            }
            out.push(Block {
                text: item.trim_end().to_string(),
                list: Some(kind),
            });
        }
    }

    /// A toggle: `<details>` with its text as the summary, its content as
    /// Markdown inside.
    fn toggle(&mut self, details: &Element, cx: &Cx) -> Block {
        let summary = details
            .elements()
            .find(|e| e.name == "summary")
            .map(|s| s.text().split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let rest: Vec<Node> = details
            .children
            .iter()
            .filter(|n| !matches!(n, Node::Element(e) if e.name == "summary"))
            .cloned()
            .collect();
        let inner = self.sub_blocks(&rest, cx);
        let mut s = format!("<details>\n<summary>{}</summary>", html_escaped(&summary));
        if !inner.is_empty() {
            s.push_str("\n\n");
            s.push_str(&inner);
        }
        s.push_str("\n\n</details>");
        Block::text(s)
    }

    fn figure(&mut self, e: &Element, cx: &Cx, out: &mut Vec<Block>) {
        let caption = e
            .elements()
            .find(|c| c.name == "figcaption")
            .map(|c| paragraph(&self.inline(&c.children, cx)))
            .filter(|c| !c.is_empty());
        let with_caption = |s: String| match &caption {
            Some(c) => format!("{s}\n*{}*", c.replace('\n', " ")),
            None => s,
        };
        if e.has_class("callout") {
            let divs: Vec<&Element> = e.elements().filter(|c| c.name == "div").collect();
            let icon = (divs.len() > 1)
                .then(|| divs[0].text().trim().to_string())
                .filter(|i| !i.is_empty() && i.chars().count() <= 4);
            let content: Vec<Node> = match divs.last() {
                Some(d) if divs.len() > 1 => d.children.clone(),
                _ => e.children.clone(),
            };
            let inner = self.sub_blocks(&content, cx);
            let mut s = format!(
                "> [!{}]{}",
                callout_type(e),
                icon.map(|i| format!(" {i}")).unwrap_or_default()
            );
            if !inner.is_empty() {
                s.push('\n');
                s.push_str(&quoted(&inner));
            }
            out.push(Block::text(s));
        } else if e.has_class("link-to-page") {
            if let Some(a) = e.find(&|c| c.name == "a") {
                let l = self.link(a, cx, Marks::default());
                out.push(Block::text(l));
            }
        } else if e.has_class("equation") {
            let tex = e
                .find(&|c| c.name == "annotation")
                .map(|a| a.text())
                .unwrap_or_else(|| e.text());
            self.lost(
                "Equations (kept as TeX: notekpr doesn't show them as math)",
                cx,
            );
            out.push(Block::text(format!("$$\n{}\n$$", tex.trim())));
        } else if let Some(b) = e.find(&|c| c.has_class("bookmark")) {
            let href = b.attr("href").unwrap_or("").to_string();
            let title = b
                .find(&|c| c.has_class("bookmark-title"))
                .map(|t| t.text().trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| href.clone());
            out.push(Block::text(with_caption(external_link(
                &escaped(&title),
                &href,
            ))));
        } else if let Some(img) = e.find(&|c| c.name == "img") {
            let src = img.attr("src").unwrap_or("").to_string();
            let s = self.embed(&src, "", cx);
            out.push(Block::text(with_caption(s)));
        } else if let Some(a) = e.find(&|c| c.name == "a" && c.attr("href").is_some()) {
            let l = self.link(a, cx, Marks::default());
            out.push(Block::text(with_caption(l)));
        } else if let Some(m) = e.find(&|c| matches!(c.name.as_str(), "video" | "audio" | "source"))
        {
            self.block(&m.clone(), cx, out);
        } else {
            self.flow(&e.children, cx, out);
        }
    }

    /// A picture, video or other file shown in a note.
    fn embed(&mut self, src: &str, alt: &str, cx: &Cx) -> String {
        match self.file_target(cx, src) {
            Some(path) => {
                let target = self.target_for(&path);
                let shown = paths::media(&path).is_some() || is_picture(&path);
                format!("{}[[{target}]]", if shown { "!" } else { "" })
            }
            None if external(src) => {
                self.lost("Pictures from the web (notekpr doesn't load them)", cx);
                format!("![{}](<{src}>)", escaped(alt))
            }
            None => {
                self.lost("Files missing from the export", cx);
                escaped(alt)
            }
        }
    }

    fn simple_table(&mut self, table: &Element, cx: &Cx) -> Option<String> {
        let mut trs = Vec::new();
        table.find_all(&|e| e.name == "tr", &mut trs);
        if trs.is_empty() {
            return None;
        }
        let header = table.find(&|e| e.name == "thead").is_some()
            || trs[0].elements().all(|c| c.name == "th");
        let mut rows: Vec<Vec<String>> = Vec::new();
        for tr in &trs {
            let cells: Vec<String> = tr
                .elements()
                .filter(|c| c.name == "td" || c.name == "th")
                .map(|c| cell_text(&self.inline(&c.children, cx)))
                .collect();
            rows.push(cells);
        }
        let width = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
        for r in &mut rows {
            r.resize(width, String::new());
        }
        let line = |cells: &[String]| format!("| {} |", cells.join(" | "));
        let mut out = Vec::new();
        let body = if header {
            out.push(line(&rows[0]));
            &rows[1..]
        } else {
            out.push(line(&vec![String::new(); width]));
            &rows[..]
        };
        out.push(format!("|{}", "---|".repeat(width)));
        for r in body {
            out.push(line(r));
        }
        Some(out.join("\n"))
    }

    /// A database shown in a page: its own (as a checklist or a table), or
    /// a linked view of one (a link to it).
    fn collection(&mut self, e: &Element, cx: &Cx, out: &mut Vec<Block>) {
        let ws = self.ws;
        let title = e
            .find(&|c| c.has_class("collection-title"))
            .map(|t| t.text().trim().to_string())
            .unwrap_or_default();
        let id = e.attr("id").and_then(text::notion_id);
        if let Some(db) = id.and_then(|id| ws.databases.get(&id))
            && db.host.as_deref() == Some(cx.page.as_str())
        {
            if !title.is_empty() {
                out.push(Block::text(format!("**{}**", escaped(&title))));
            }
            let block = match self.shapes[&db.id] {
                Shape::Checklist => self.checklist(db, cx),
                Shape::Notes => Some(self.table(db, true, cx)),
                Shape::Table | Shape::Folder => Some(self.table(db, false, cx)),
            };
            out.extend(block);
            return;
        }
        let view = e
            .find(&|c| c.name == "table")
            .map(view_of)
            .unwrap_or_default();
        let target = self.source_of(&view).and_then(|db| {
            let d = &ws.databases[&db];
            match self.note_of(&db) {
                Some(_) => Some(db),
                None => d.host.clone(),
            }
        });
        match target {
            Some(t) => {
                let l = self.link_to(&t, &title, cx);
                out.push(Block::text(l));
            }
            None => {
                self.lost("Linked views of databases that aren't in the export", cx);
                if !title.is_empty() {
                    out.push(Block::text(escaped(&title)));
                }
            }
        }
    }

    /// A database's columns: as its page (or the page showing it) shows
    /// them, or else those of its CSV that any row fills.
    fn columns(&self, db: &Database) -> Vec<String> {
        if let Some(v) = db.view.as_ref().filter(|v| !v.columns.is_empty()) {
            return v.columns.clone();
        }
        db.columns
            .iter()
            .enumerate()
            .filter(|&(i, _)| {
                i == 0
                    || db
                        .csv_rows
                        .iter()
                        .any(|r| r.get(i).is_some_and(|v| !v.trim().is_empty()))
            })
            .map(|(_, c)| c.clone())
            .collect()
    }

    fn title_column(db: &Database) -> String {
        db.columns.first().cloned().unwrap_or_else(|| "Name".into())
    }

    /// A row's title: a link to its note, or its text.
    fn row_title(&mut self, row: &Page, link: bool, cx: &Cx) -> String {
        if link && self.note_of(&row.id).is_some() {
            self.link_to(&row.id, &row.title, cx)
        } else {
            escaped(&row.title)
        }
    }

    fn table(&mut self, db: &Database, link: bool, cx: &Cx) -> Block {
        let ws = self.ws;
        let columns = self.columns(db);
        let title_col = Self::title_column(db);
        let mut lines = vec![
            format!(
                "| {} |",
                columns
                    .iter()
                    .map(|c| cell_text(&escaped(c)))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            format!("|{}", "---|".repeat(columns.len().max(1))),
        ];
        for r in &db.rows {
            let row = &ws.pages[r];
            let cells: Vec<String> = columns
                .iter()
                .map(|c| {
                    let s = if *c == title_col {
                        self.row_title(row, link, cx)
                    } else {
                        row.props
                            .iter()
                            .find(|p| p.name == *c)
                            .map(|p| self.display(p, cx))
                            .unwrap_or_default()
                    };
                    cell_text(&s)
                })
                .collect();
            lines.push(format!("| {} |", cells.join(" | ")));
        }
        self.missing_rows(db, cx);
        Block::text(lines.join("\n"))
    }

    fn missing_rows(&mut self, db: &Database, cx: &Cx) {
        if db.csv_rows.len() > db.rows.len() {
            self.lost("Rows whose pages weren't in the export", cx);
        }
    }

    /// A database's rows as tasks: ticked when done, then what else the
    /// view shows (a status only when it's more than done or not).
    fn checklist(&mut self, db: &Database, cx: &Cx) -> Option<Block> {
        let ws = self.ws;
        let columns = self.columns(db);
        let title_col = Self::title_column(db);
        let mut lines = Vec::new();
        for r in &db.rows {
            let row = &ws.pages[r];
            let (done, said) = tick(row);
            let title = self.row_title(row, true, cx);
            let mut item = format!("- [{}] {title}", if done { 'x' } else { ' ' });
            // A status the tick doesn't say (in progress, cancelled) is
            // written, even where the view didn't show it.
            if said.is_none()
                && let Some(p) = row.props.iter().find(|p| p.kind == "status")
                && !columns.contains(&p.name)
            {
                let v = self.display(p, cx);
                item.push_str(&format!(" · {v}"));
            }
            for c in &columns {
                if *c == title_col || said.as_deref() == Some(c.as_str()) {
                    continue;
                }
                let Some(p) = row.props.iter().find(|p| p.name == *c) else {
                    continue;
                };
                let v = self.display(p, cx);
                if !v.trim().is_empty() {
                    item.push_str(" · ");
                    item.push_str(&v.replace('\n', " "));
                }
            }
            lines.push(item);
        }
        self.missing_rows(db, cx);
        (!lines.is_empty()).then(|| Block {
            text: lines.join("\n"),
            list: Some(ListKind::Task),
        })
    }

    // ------------------------------------------------------------ inline

    fn inline(&mut self, nodes: &[Node], cx: &Cx) -> String {
        self.inline_with(nodes, cx, Marks::default())
    }

    fn inline_with(&mut self, nodes: &[Node], cx: &Cx, m: Marks) -> String {
        let mut out = String::new();
        for n in nodes {
            match n {
                Node::Text(t) => escape(t, &mut out),
                Node::Element(e) => {
                    let s = self.inline_element(e, cx, m);
                    out.push_str(&s);
                }
            }
        }
        out
    }

    fn inline_element(&mut self, e: &Element, cx: &Cx, m: Marks) -> String {
        let inner = |w: &mut Self, m: Marks| w.inline_with(&e.children, cx, m);
        match e.name.as_str() {
            "strong" | "b" if !m.bold => wrap("**", &inner(self, Marks { bold: true, ..m })),
            "em" | "i" if !m.italic => wrap("*", &inner(self, Marks { italic: true, ..m })),
            "del" | "s" if !m.strike => wrap("~~", &inner(self, Marks { strike: true, ..m })),
            "code" => text::code_span(&e.text().replace('\n', " ")),
            "u" => wrap_html("u", &inner(self, m)),
            "sub" | "sup" | "kbd" => wrap_html(&e.name, &inner(self, m)),
            "mark" => colored(e, inner(self, m)),
            "a" => self.link(e, cx, m),
            "time" => match e.attr("datetime") {
                Some(d) => text::date_mention(d),
                None => escaped(&e.text()),
            },
            "br" => "\n".into(),
            "input" => String::new(),
            "img" => match e.attr("src") {
                Some(src) if !external(src) => {
                    let src = src.to_string();
                    let alt = e.attr("alt").unwrap_or("").to_string();
                    self.embed(&src, &alt, cx)
                }
                _ => escaped(e.attr("alt").unwrap_or("")),
            },
            "span" if e.has_class("icon") => String::new(),
            "span" if e.has_class("notion-text-equation-token") => {
                self.lost(
                    "Equations (kept as TeX: notekpr doesn't show them as math)",
                    cx,
                );
                let tex = e
                    .find(&|c| c.name == "annotation")
                    .map(|a| a.text())
                    .unwrap_or_else(|| e.text());
                format!("${}$", tex.trim())
            }
            "span" => {
                let s = inner(self, m);
                let s = if e
                    .attr("style")
                    .is_some_and(|st| st.contains("border-bottom:0.05em solid"))
                {
                    wrap_html("u", &s)
                } else {
                    s
                };
                colored(e, s)
            }
            _ => inner(self, m),
        }
    }

    fn link(&mut self, a: &Element, cx: &Cx, m: Marks) -> String {
        let href = a.attr("href").unwrap_or("").to_string();
        let text = label(a);
        let page = a
            .attr("data-notion-page-id")
            .and_then(text::notion_id)
            .or_else(|| self.page_of_href(cx, &href));
        if let Some(id) = page {
            return self.link_to(&id, &text, cx);
        }
        if href.is_empty() || href.starts_with('#') {
            return self.inline_with(&a.children, cx, m);
        }
        if !external(&href) {
            return match self.file_target(cx, &href) {
                Some(path) => self.wiki(&path, &text),
                None => self.inline_with(&a.children, cx, m),
            };
        }
        let inner = self.inline_with(&a.children, cx, m);
        if text == href && !href.contains([' ', '<', '>']) {
            format!("<{href}>")
        } else {
            external_link(&inner, &href)
        }
    }

    // -------------------------------------------------------- properties

    /// A property as a front matter value.
    fn value(&mut self, prop: &Prop, cx: &Cx) -> Option<Value> {
        let cell = &prop.cell;
        let plain = cell.text().split_whitespace().collect::<Vec<_>>().join(" ");
        let v = match prop.kind.as_str() {
            "title" | "button" | "verification" => return None,
            "multi_select" => Value::List(selected(cell).into_iter().map(Value::Str).collect()),
            "select" => Value::Str(selected(cell).into_iter().next()?),
            "status" => Value::Str(status(cell)?.0),
            "checkbox" => Value::Raw(checked(cell).to_string()),
            "date" | "created_time" | "last_edited_time" => match times(cell).as_slice() {
                [] => Value::Str(plain),
                [one] => Value::Raw(text::iso(one)),
                [from, to, ..] => Value::Raw(format!("{}/{}", text::iso(from), text::iso(to))),
            },
            "relation" | "file" | "files" => {
                let mut links = Vec::new();
                cell.find_all(&|e| e.name == "a", &mut links);
                let links: Vec<Element> = links.into_iter().cloned().collect();
                Value::List(
                    links
                        .iter()
                        .map(|a| Value::Str(self.link(a, cx, Marks::default())))
                        .collect(),
                )
            }
            "url" => Value::Str(
                cell.find(&|e| e.name == "a")
                    .and_then(|a| a.attr("href"))
                    .map(str::to_string)
                    .unwrap_or(plain),
            ),
            "number" | "formula" | "rollup" | "unique_id" if text::is_number(&plain) => {
                Value::Raw(plain)
            }
            "formula" | "rollup" if times(cell).len() == 1 => {
                Value::Raw(text::iso(&times(cell)[0]))
            }
            _ => Value::Str(plain),
        };
        match &v {
            Value::List(l) if l.is_empty() => None,
            Value::Str(s) | Value::Raw(s) if s.is_empty() => None,
            _ => Some(v),
        }
    }

    /// A property as a table cell or after a task shows it.
    fn display(&mut self, prop: &Prop, cx: &Cx) -> String {
        let cell = &prop.cell;
        match prop.kind.as_str() {
            "title" => String::new(),
            "date" | "created_time" | "last_edited_time" => {
                let t = times(cell);
                if t.is_empty() {
                    escaped(cell.text().trim())
                } else {
                    t.iter()
                        .map(|d| text::date_mention(d))
                        .collect::<Vec<_>>()
                        .join(" → ")
                }
            }
            "checkbox" => if checked(cell) { "✓" } else { "" }.into(),
            "multi_select" | "select" => selected(cell)
                .iter()
                .map(|s| escaped(s))
                .collect::<Vec<_>>()
                .join(", "),
            "status" => status(cell).map(|s| escaped(&s.0)).unwrap_or_default(),
            "relation" | "file" | "files" => {
                let mut links = Vec::new();
                cell.find_all(&|e| e.name == "a", &mut links);
                let links: Vec<Element> = links.into_iter().cloned().collect();
                links
                    .iter()
                    .map(|a| self.link(a, cx, Marks::default()))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
            _ => self.inline(&cell.children, cx),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Marks {
    bold: bool,
    italic: bool,
    strike: bool,
}

fn wrap_html(tag: &str, s: &str) -> String {
    if s.trim().is_empty() {
        s.to_string()
    } else {
        format!("<{tag}>{s}</{tag}>")
    }
}

fn external_link(text: &str, href: &str) -> String {
    let text = if text.trim().is_empty() {
        escaped(href)
    } else {
        text.to_string()
    };
    if href.contains([' ', '(', ')', '<', '>']) {
        format!(
            "[{text}](<{}>)",
            href.replace('<', "%3C").replace('>', "%3E")
        )
    } else {
        format!("[{text}]({href})")
    }
}

/// A wiki link to `target` (named `name`) showing `text`, unless that is
/// its name (or the same day, for a date).
fn alias(target: &str, name: &str, text: &str) -> String {
    let text = text.replace(['[', ']', '|'], "").trim().to_string();
    let same = text.is_empty()
        || text == name
        || text::english_date(&text).is_some_and(|d| d.to_string() == name);
    if same {
        format!("[[{target}]]")
    } else {
        format!("[[{target}|{text}]]")
    }
}

/// Whether a row is done, and the property that said so (left out of
/// what's written after it): a checkbox, or a status whose color is
/// Notion's for done (green) or not started (none, gray).
fn tick(row: &Page) -> (bool, Option<String>) {
    if let Some(p) = row.props.iter().find(|p| p.kind == "checkbox") {
        return (checked(&p.cell), Some(p.name.clone()));
    }
    if let Some(p) = row.props.iter().find(|p| p.kind == "status") {
        let color = status(&p.cell).and_then(|s| s.1);
        let done = color.as_deref() == Some("green");
        let plain = done || matches!(color.as_deref(), None | Some("default" | "gray"));
        return (done, plain.then(|| p.name.clone()));
    }
    (false, None)
}

/// A row's properties in its database's order (the CSV's columns).
fn ordered<'p>(page: &'p Page, db: &Database) -> Vec<&'p Prop> {
    let mut out: Vec<&Prop> = db
        .columns
        .iter()
        .filter_map(|c| page.props.iter().find(|p| p.name == *c))
        .collect();
    for p in &page.props {
        if !out.iter().any(|o| std::ptr::eq(*o, p)) {
            out.push(p);
        }
    }
    out
}

/// At most ten of `items`, then how many more.
fn short_list(items: &[String]) -> String {
    let shown = items
        .iter()
        .take(10)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > 10 {
        format!("{shown} and {} more", items.len() - 10)
    } else {
        shown
    }
}

/// Whether a file is a picture a note shows (`![[x.png]]`), by its name.
fn is_picture(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase());
    matches!(
        ext.as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" | "ico")
    )
}

/// A code block, fenced, with its language.
fn code_block(pre: &Element) -> String {
    let code = pre.find(&|e| e.name == "code").unwrap_or(pre);
    let lang = pre
        .attr("data-notion-code-syntax")
        .map(str::to_string)
        .or_else(|| {
            code.classes()
                .find_map(|c| c.strip_prefix("language-"))
                .map(str::to_string)
        })
        .map(|l| language(&l))
        .unwrap_or_default();
    let text = code.text();
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let fence = "`".repeat((text::longest_run(text, '`') + 1).max(3));
    format!("{fence}{lang}\n{text}\n{fence}")
}
