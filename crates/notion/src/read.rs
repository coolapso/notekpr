//! Reading exports: every page with its properties and body, every
//! database with its rows (and the templates that aren't rows), where each
//! page sits, and the files.
//!
//! An export is laid out by title and ID: `Diary <id>.html` is a page,
//! `Diary/` holds what's inside it (subpages, files, the rows of the
//! database it is), and `Diary <id>.csv` beside it makes it a database's
//! page. An inline database's CSV sits in its page's folder, named after
//! the database and the block showing it: `Diary/@May 2, 2023/Today <id>.csv`,
//! its rows in `Diary/@May 2, 2023/Today/`. A database only shown in a
//! linked view comes as its CSV and rows, without a page.

use std::collections::BTreeMap;

use notekpr_core::paths;

use crate::dom::{self, Element, Node};
use crate::{Files, csv, text};

pub struct Page {
    pub id: String,
    /// The export it came in (of those read together), and its path there.
    pub export: usize,
    pub path: String,
    /// Where its subpages and files are: its path without ` <id>.html`.
    pub folder: String,
    /// Its title as it reads (`@November 13, 2023`).
    pub title: String,
    /// Its title with dates as ISO dates (`2023-11-13`): what its name is
    /// made of.
    pub title_iso: String,
    /// Its title as its database's CSV writes it: links as their address.
    pub csv_title: String,
    pub icon: Option<String>,
    /// It had an icon of its own (an image), which doesn't come along.
    pub image_icon: bool,
    pub cover: bool,
    pub description: Option<Element>,
    pub props: Vec<Prop>,
    pub body: Element,
}

/// A property of a database's row, as its page shows it.
pub struct Prop {
    pub name: String,
    /// Notion's type: `text`, `multi_select`, `date`, `relation`, …
    pub kind: String,
    pub cell: Element,
}

pub struct Database {
    /// The ID of its page, or of the block showing it inline.
    pub id: String,
    pub export: usize,
    pub title: String,
    /// The folder its rows' pages are in.
    pub rows_folder: String,
    /// The CSV's header and rows: every row and property, as text.
    pub columns: Vec<String>,
    pub csv_rows: Vec<Vec<String>>,
    /// Its own page (a full-page database).
    pub page: Option<String>,
    /// The page showing it inline.
    pub host: Option<String>,
    /// Its table as its page or host shows it.
    pub view: Option<View>,
    /// Its rows' pages, in the view's order (then the CSV's).
    pub rows: Vec<String>,
    /// Pages in its folder that aren't rows: its templates.
    pub templates: Vec<String>,
}

/// A table of a database's rows (in its page, inline, or a linked view).
#[derive(Clone, Default)]
pub struct View {
    pub columns: Vec<String>,
    /// The rows' page IDs, in its order.
    pub rows: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parent {
    Page(String),
    Database(String),
}

/// What the exports hold that can't be imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unreadable {
    /// A zip inside the export (Notion splits big ones): its path.
    Zip(String),
    /// A Markdown export's notes, which this doesn't read: how many.
    Markdown(usize),
}

#[derive(Default)]
pub struct Workspace {
    pub pages: BTreeMap<String, Page>,
    pub databases: BTreeMap<String, Database>,
    /// Where each page sits; pages without one are at the export's top.
    pub parents: BTreeMap<String, Parent>,
    /// Database of each template page.
    pub templates: BTreeMap<String, String>,
    /// Files other than pages and CSVs, by export and path.
    pub files: BTreeMap<(usize, String), Vec<u8>>,
    /// Pages by export and path.
    pub by_path: BTreeMap<(usize, String), String>,
    pub unreadable: Vec<Unreadable>,
}

/// A path in an export as a vault path would be: `/`-separated, composed
/// accents (zips made on macOS hold decomposed ones).
fn clean_path(p: &str) -> String {
    paths::nfc(&p.replace('\\', "/"))
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

/// `name <id>` → (`name`, `id`).
fn split_id(stem: &str) -> Option<(&str, String)> {
    let id = text::notion_id(stem)?;
    let name = stem.get(..stem.len().checked_sub(32)?)?.trim_end();
    Some((name, id))
}

impl Workspace {
    pub fn read(exports: Vec<Files>) -> Workspace {
        let mut ws = Workspace::default();
        let mut csvs = Vec::new();
        let mut markdown = 0;
        for (export, files) in exports.into_iter().enumerate() {
            for (path, data) in files {
                let path = clean_path(&path);
                let hidden = path
                    .split('/')
                    .any(|s| s.starts_with('.') || s == "__MACOSX");
                if path.is_empty() || path.ends_with('/') || hidden {
                    continue;
                }
                let lower = path.to_lowercase();
                if lower.ends_with(".html") {
                    if let Some(page) = Page::read(export, &path, &String::from_utf8_lossy(&data))
                        && !ws.pages.contains_key(&page.id)
                    {
                        ws.by_path.insert((export, path), page.id.clone());
                        ws.pages.insert(page.id.clone(), page);
                    }
                } else if lower.ends_with(".csv") {
                    csvs.push((export, path, data));
                } else if lower.ends_with(".zip") {
                    ws.unreadable.push(Unreadable::Zip(path));
                } else if lower.ends_with(".md") {
                    markdown += 1;
                } else {
                    ws.files.insert((export, path), data);
                }
            }
        }
        if markdown > 0 && ws.pages.is_empty() {
            ws.unreadable.push(Unreadable::Markdown(markdown));
        }
        csvs.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        for (export, path, data) in csvs {
            ws.read_database(export, &path, &String::from_utf8_lossy(&data));
        }
        ws.find_rows();
        ws.find_parents();
        ws
    }

    /// The page in `export` whose folder is `folder`.
    fn page_with_folder(&self, export: usize, folder: &str) -> Option<&Page> {
        self.pages
            .values()
            .find(|p| p.export == export && p.folder == folder)
    }

    fn read_database(&mut self, export: usize, path: &str, csv_text: &str) {
        let stem = &path[..path.len() - 4];
        // A Markdown export's `X <id>_all.csv` has every row; `X <id>.csv`
        // only those of the view.
        let (stem, all) = match stem.strip_suffix("_all") {
            Some(s) => (s, true),
            None => (stem, false),
        };
        let (dir, file) = stem.rsplit_once('/').unwrap_or(("", stem));
        let Some((name, id)) = split_id(file) else {
            return;
        };
        let mut rows = csv::parse(csv_text);
        if rows.is_empty() {
            return;
        }
        let columns = rows.remove(0);
        if let Some(db) = self.databases.get_mut(&id) {
            if all {
                db.columns = columns;
                db.csv_rows = rows;
            }
            return;
        }
        let mut db = Database {
            id: id.clone(),
            export,
            title: name.to_string(),
            rows_folder: paths::join(dir, name),
            columns,
            csv_rows: rows,
            page: None,
            host: None,
            view: None,
            rows: Vec::new(),
            templates: Vec::new(),
        };
        let own = self
            .by_path
            .get(&(export, format!("{stem}.html")))
            .and_then(|pid| self.pages.get(pid));
        if let Some(page) = own {
            db.page = Some(page.id.clone());
            db.title = page.title.clone();
            db.view = page
                .body
                .find(&|e| e.name == "table" && e.has_class("collection-content"))
                .map(view_of);
        } else if let Some(host) = self.page_with_folder(export, dir)
            && let Some(block) = host.body.find(&|e| {
                e.has_class("collection-content")
                    && e.attr("id").and_then(text::notion_id).as_deref() == Some(id.as_str())
            })
        {
            db.host = Some(host.id.clone());
            let title = block
                .find(&|e| e.has_class("collection-title"))
                .map(|t| t.text().trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| name.to_string());
            db.title = title.clone();
            db.view = block.find(&|e| e.name == "table").map(view_of);
        }
        self.databases.insert(id, db);
    }

    /// Each database's rows and templates: the pages in its folder that its
    /// table or its CSV lists, and the others.
    fn find_rows(&mut self) {
        let mut found = Vec::new();
        for db in self.databases.values() {
            let in_view: BTreeMap<&str, usize> = db
                .view
                .iter()
                .flat_map(|v| v.rows.iter().enumerate())
                .map(|(i, id)| (id.as_str(), i))
                .collect();
            let mut in_csv: BTreeMap<String, usize> = BTreeMap::new();
            for (i, r) in db.csv_rows.iter().enumerate() {
                if let Some(name) = r.first() {
                    in_csv.entry(name.trim().to_string()).or_insert(i);
                }
            }
            let mut rows = Vec::new();
            let mut templates = Vec::new();
            for p in self.pages.values() {
                if p.export != db.export || paths::parent(&p.path) != db.rows_folder {
                    continue;
                }
                let key = (
                    in_view.get(p.id.as_str()).copied().unwrap_or(usize::MAX),
                    in_csv
                        .get(p.title.trim())
                        .or_else(|| in_csv.get(p.csv_title.trim()))
                        .copied()
                        .unwrap_or(usize::MAX),
                    p.path.clone(),
                );
                if key.0 == usize::MAX && key.1 == usize::MAX {
                    templates.push(p.id.clone());
                } else {
                    rows.push((key, p.id.clone()));
                }
            }
            rows.sort();
            found.push((
                db.id.clone(),
                rows.into_iter().map(|(_, id)| id).collect(),
                templates,
            ));
        }
        for (id, rows, templates) in found {
            for t in &templates {
                self.templates.insert(t.clone(), id.clone());
            }
            let db = self.databases.get_mut(&id).expect("just read");
            db.rows = rows;
            db.templates = templates;
        }
    }

    fn find_parents(&mut self) {
        let mut row_of = BTreeMap::new();
        for db in self.databases.values() {
            for r in &db.rows {
                row_of.insert(r.clone(), db.id.clone());
            }
        }
        let folders: BTreeMap<(usize, &str), &str> = self
            .pages
            .values()
            .map(|p| ((p.export, p.folder.as_str()), p.id.as_str()))
            .collect();
        let mut parents = BTreeMap::new();
        for p in self.pages.values() {
            if let Some(db) = row_of.get(&p.id) {
                parents.insert(p.id.clone(), Parent::Database(db.clone()));
            } else if let Some(owner) = folders.get(&(p.export, paths::parent(&p.path)))
                && *owner != p.id
            {
                parents.insert(p.id.clone(), Parent::Page(owner.to_string()));
            }
        }
        self.parents = parents;
    }

    /// The database a page is the page of.
    pub fn database_of_page(&self, page: &str) -> Option<&Database> {
        self.databases
            .values()
            .find(|d| d.page.as_deref() == Some(page))
    }

    /// The database a row belongs to.
    pub fn database_of_row(&self, page: &str) -> Option<&Database> {
        match self.parents.get(page) {
            Some(Parent::Database(db)) => self.databases.get(db),
            _ => None,
        }
    }
}

/// A table of rows: its column names and its rows' IDs.
pub fn view_of(table: &Element) -> View {
    let mut columns = Vec::new();
    if let Some(head) = table.find(&|e| e.name == "thead") {
        let mut ths = Vec::new();
        head.find_all(&|e| e.name == "th", &mut ths);
        columns = ths.iter().map(|th| th.text().trim().to_string()).collect();
    }
    let mut trs = Vec::new();
    if let Some(body) = table.find(&|e| e.name == "tbody") {
        body.find_all(&|e| e.name == "tr", &mut trs);
    }
    let rows = trs
        .iter()
        .filter_map(|tr| {
            tr.find(&|e| e.attr("data-notion-page-id").is_some())
                .and_then(|a| a.attr("data-notion-page-id"))
                .or(tr.attr("id"))
                .and_then(text::notion_id)
        })
        .collect();
    View { columns, rows }
}

impl Page {
    fn read(export: usize, path: &str, html: &str) -> Option<Page> {
        let root = dom::parse(html);
        let article = root.find(&|e| e.name == "article")?;
        let id = text::notion_id(article.attr("id")?)?;
        let header = article.elements().find(|e| e.name == "header");
        let find = |f: &dyn Fn(&Element) -> bool| header.and_then(|h| h.find(f));
        let title_el = find(&|e| e.has_class("page-title"));
        let mut title = title_el
            .map(|t| t.text().trim().to_string())
            .unwrap_or_default();
        let mut title_iso = title_el
            .map(|t| iso_text(t).trim().to_string())
            .unwrap_or_default();
        let csv_title = title_el
            .map(|t| csv_text(t).trim().to_string())
            .unwrap_or_default();
        let header_icon = find(&|e| e.has_class("page-header-icon"));
        let icon = article
            .attr("data-notion-page-icon")
            .or_else(|| {
                header_icon
                    .and_then(|h| h.find(&|e| e.attr("data-emoji").is_some()))
                    .and_then(|e| e.attr("data-emoji"))
            })
            .map(str::trim)
            .filter(|i| !i.is_empty())
            .map(str::to_string);
        let image_icon =
            icon.is_none() && header_icon.is_some_and(|h| h.find(&|e| e.name == "img").is_some());
        let cover = find(&|e| e.has_class("page-cover-image")).is_some();
        let description = find(&|e| e.has_class("page-description"))
            .filter(|d| !d.text().trim().is_empty())
            .cloned();
        let mut rows = Vec::new();
        if let Some(t) = find(&|e| e.has_class("properties")) {
            t.find_all(&|e| e.has_class("property-row"), &mut rows);
        }
        let props = rows
            .iter()
            .filter_map(|tr| {
                let kind = tr.classes().find_map(|c| c.strip_prefix("property-row-"))?;
                let name = tr.elements().find(|e| e.name == "th")?.text();
                let cell = tr.elements().find(|e| e.name == "td")?.clone();
                Some(Prop {
                    name: name.trim().to_string(),
                    kind: kind.to_string(),
                    cell,
                })
            })
            .collect();
        let body = article
            .find(&|e| e.has_class("page-body"))
            .cloned()
            .unwrap_or_default();
        let stem = path.strip_suffix(".html").unwrap_or(path);
        let (dir, file) = stem.rsplit_once('/').unwrap_or(("", stem));
        let named = split_id(file).filter(|(name, fid)| *fid == id && !name.is_empty());
        let folder = match named {
            Some((name, _)) => paths::join(dir, name),
            None => stem.to_string(),
        };
        // A title made of a mention Notion can't write out (`‣`): the
        // name its file got.
        if !title.chars().any(char::is_alphanumeric)
            && let Some((name, _)) = named
            && name.chars().any(char::is_alphanumeric)
        {
            title = name.to_string();
            title_iso = name.to_string();
        }
        Some(Page {
            id,
            export,
            path: path.to_string(),
            folder,
            title,
            title_iso,
            csv_title,
            icon,
            image_icon,
            cover,
            description,
            props,
            body,
        })
    }

    /// Whether it says anything: more than empty paragraphs.
    pub fn has_content(&self) -> bool {
        content(&self.body)
    }
}

fn content(e: &Element) -> bool {
    e.children.iter().any(|n| match n {
        Node::Text(t) => !t.trim().is_empty(),
        Node::Element(c) => match c.name.as_str() {
            "script" | "link" | "style" | "meta" | "br" => false,
            "p" | "div" | "span" => content(c),
            _ => true,
        },
    })
}

/// An element's text with its links as their addresses, as a CSV writes
/// it.
fn csv_text(e: &Element) -> String {
    let mut out = String::new();
    for n in &e.children {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Element(c) if c.name == "a" && c.attr("data-notion-page-id").is_none() => {
                match c.attr("href") {
                    Some(h) => out.push_str(h),
                    None => out.push_str(&c.text()),
                }
            }
            Node::Element(c) => out.push_str(&csv_text(c)),
        }
    }
    out
}

/// An element's text with its dates (`<time datetime>`) as ISO dates.
fn iso_text(e: &Element) -> String {
    let mut out = String::new();
    for n in &e.children {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Element(c) if c.name == "time" => match c.attr("datetime") {
                Some(d) => out.push_str(d.split('T').next().unwrap_or(d)),
                None => out.push_str(&c.text()),
            },
            Node::Element(c) => out.push_str(&iso_text(c)),
        }
    }
    out
}
