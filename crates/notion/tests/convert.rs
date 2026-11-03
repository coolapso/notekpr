//! A small export, laid out and written the way Notion's HTML exports are
//! (checked against real ones), converted as an import would.

use std::collections::BTreeMap;

use notekpr_notion::{Converted, Files, Options, REPORT, convert};

fn id(n: u32) -> String {
    format!("{n:032x}")
}

fn uuid(n: u32) -> String {
    let s = id(n);
    format!(
        "{}-{}-{}-{}-{}",
        &s[..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..]
    )
}

fn page(n: u32, icon: &str, title: &str, props: &str, body: &str) -> Vec<u8> {
    format!(
        "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"/>\
         <title>x</title><style>a > b {{ c: '<p>' }}</style></head><body>\
         <article id=\"{}\" class=\"page sans\"{}><header>\
         <h1 class=\"page-title\" dir=\"auto\">{title}</h1>\
         <p class=\"page-description\" dir=\"auto\"></p>\
         <table class=\"properties\"><tbody>{props}</tbody></table></header>\
         <div class=\"page-body\">{body}</div></article></body></html>",
        uuid(n),
        if icon.is_empty() {
            String::new()
        } else {
            format!(" data-notion-page-icon=\"{icon}\"")
        }
    )
    .into_bytes()
}

fn prop(kind: &str, name: &str, cell: &str) -> String {
    format!(
        "<tr class=\"property-row property-row-{kind}\"><th><span class=\"icon property-icon\">\
         <img src=\"https://app.notion.com/icons/x.svg\"/></span>{name}</th><td>{cell}</td></tr>"
    )
}

fn status(name: &str, color: &str) -> String {
    let c = if color.is_empty() {
        String::new()
    } else {
        format!(" select-value-color-{color}")
    };
    prop(
        "status",
        "Status",
        &format!("<span class=\"status-value{c}\"><div class=\"status-dot\"></div>{name}</span>"),
    )
}

fn link(n: u32, href: &str, text: &str) -> String {
    format!(
        "<a href=\"{href}\" data-notion-page-id=\"{}\"><span class=\"icon\" data-emoji=\"🗓️\"></span>{text}</a>",
        uuid(n)
    )
}

/// A table of rows, as a database's page or an inline database shows it.
fn table(columns: &[&str], rows: &[(u32, &str, &str)]) -> String {
    let head: String = columns.iter().map(|c| format!("<th>{c}</th>")).collect();
    let body: String = rows
        .iter()
        .map(|(n, href, title)| {
            format!(
                "<tr id=\"{}\"><td class=\"cell-title\"><a href=\"{href}\" data-notion-page-id=\"{}\">{title}</a></td></tr>",
                uuid(*n),
                uuid(*n)
            )
        })
        .collect();
    format!(
        "<table class=\"collection-content\"><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>"
    )
}

const JOURNAL: u32 = 1;
const ENTRY: u32 = 2;
const READING: u32 = 3;
const TEMPLATE: u32 = 4;
const TODAY: u32 = 5;
const WRITE_TESTS: u32 = 6;
const BUY_MILK: u32 = 7;
const CALL_BOB: u32 = 8;
const WEEK: u32 = 9;
const TRIPS: u32 = 10;
const MEXICO: u32 = 11;
const SCHEDULE: u32 = 12;
const DAY_ONE: u32 = 13;
const TASKS: u32 = 14;
const OLD_TASK: u32 = 15;

/// A full-page database (Journal) whose entry has a database of its own
/// (Today), a row whose title holds a link, and a template.
fn journal() -> Files {
    let entry_body = format!(
        "<h2 id=\"h\">To do</h2>\
         <div id=\"{today}\" class=\"collection-content\"><h4 class=\"collection-title\">Today</h4>\
         <div class=\"collection-content-wrapper\">{today_table}</div></div>\
         <p class=\"\"></p><p class=\"\">See {reading} on <time datetime=\"2023-05-03\">@May 3, 2023</time>, \
         <strong><strong>snake_case</strong></strong> and *stars*, {week}.</p>\
         <p class=\"block-color-gray\">Gray <mark class=\"highlight-red_background\">red</mark></p>\
         <figure id=\"f\" class=\"image\"><a href=\"%40May%202%2C%202023/photo.png\">\
         <img src=\"%40May%202%2C%202023/photo.png\"/></a><figcaption>A photo</figcaption></figure>\
         <figure class=\"block-color-yellow_background callout\"><div style=\"font-size:1.5em\">\
         <span class=\"icon\">💡</span></div><div style=\"width:100%\">Mind <em>this</em><p>Second</p></div></figure>\
         <ul class=\"toggle\"><li><details open=\"\"><summary>More &amp; less</summary><p>Hidden</p></details></li></ul>\
         <ol type=\"1\" start=\"1\" class=\"numbered-list\"><li>One</li></ol>\
         <ol type=\"1\" start=\"2\" class=\"numbered-list\"><li>Two<ul class=\"bulleted-list\"><li>Under</li></ul></li></ol>\
         <ul class=\"to-do-list\"><li><input type=\"checkbox\" class=\"checkbox checkbox-on\" checked=\"\"/> \
         <span class=\"to-do-children-checked\">Done thing</span><div class=\"indented\"></div></li></ul>\
         <pre class=\"code\" data-notion-code-syntax=\"Rust\"><code>fn main() {{}}\n```</code></pre>\
         <blockquote>Quoted<br/>twice</blockquote>\
         <table class=\"simple-table\"><thead><tr><th>A</th><th>B</th></tr></thead>\
         <tbody><tr><td>1 | 2</td><td>x</td></tr></tbody></table>\
         <figure><a href=\"https://example.com/x\" class=\"bookmark source\"><div class=\"bookmark-info\">\
         <div class=\"bookmark-text\"><div class=\"bookmark-title\">Example</div></div>\
         <div class=\"bookmark-href\"><img src=\"https://example.com/favicon.ico\" class=\"icon bookmark-icon\"/>https://example.com/x</div>\
         </div></a></figure>\
         <div class=\"column-list\"><div class=\"column\"><p>Left</p></div><div class=\"column\"><p>Right</p></div></div>\
         <nav class=\"block-color-gray table_of_contents\"></nav>\
         <figure class=\"equation\"><annotation encoding=\"application/x-tex\">e=mc^2</annotation></figure>\
         <hr/>\
         <div id=\"{view}\" class=\"collection-content\"><h4 class=\"collection-title\">Journal</h4>\
         <div class=\"collection-content-wrapper\">{view_table}</div></div>",
        today = uuid(TODAY),
        today_table = table(
            &["Name", "tag"],
            &[
                (
                    WRITE_TESTS,
                    "%40May%202%2C%202023/Today/Write%20tests%20x.html",
                    "Write tests"
                ),
                (
                    BUY_MILK,
                    "%40May%202%2C%202023/Today/Buy%20milk%20x.html",
                    "Buy milk"
                ),
                (
                    CALL_BOB,
                    "%40May%202%2C%202023/Today/Call%20Bob%20x.html",
                    "Call: Bob"
                ),
            ]
        ),
        reading = link(
            READING,
            &format!("Read%20x%20y%20{}.html", id(READING)),
            "the reading"
        ),
        week = link(
            WEEK,
            &format!("https://app.notion.com/p/Week-18-{}?pvs=21", id(WEEK)),
            "Week 18"
        ),
        view = uuid(99),
        view_table = table(&["Name"], &[(READING, "x.html", "Read x.y")]),
    );
    let entry_props = [
        prop(
            "date",
            "Date",
            "<time datetime=\"2023-05-02\">May 2, 2023</time>",
        ),
        prop(
            "created_time",
            "Created",
            "<time datetime=\"2023-05-01T20:52\">May 1, 2023 8:52 PM</time>",
        ),
        prop(
            "multi_select",
            "Tags",
            "<span class=\"selected-value select-value-color-purple\">Daily OPS</span>\
             <span class=\"selected-value\">Work</span>",
        ),
        prop("formula", "week", "18"),
        prop(
            "relation",
            "Week Planning",
            &link(WEEK, "https://app.notion.com/p/x", "Week 18"),
        ),
    ]
    .concat();
    let task = |n: u32, title: &str, st: String, body: &str| {
        page(
            n,
            "",
            title,
            &[st, prop("text", "tag", "work")].concat(),
            body,
        )
    };
    let mut f: Files = vec![
        (
            format!("Journal {}.html", id(JOURNAL)),
            page(
                JOURNAL,
                "📓",
                "Journal",
                "",
                &format!(
                    "<div class=\"collection-content-wrapper\">{}</div>",
                    table(
                        &["Name", "Tags", "Created"],
                        &[(ENTRY, "Journal/x.html", "@May 2, 2023")]
                    )
                ),
            ),
        ),
        (
            format!("Journal {}.csv", id(JOURNAL)),
            "\u{feff}Name,Created,Date,Tags,week,Week Planning\n\
             \"@May 2, 2023 \",\"May 1, 2023 8:52 PM\",\"May 2, 2023\",\"Daily OPS, Work\",18,Week 18 (https://app.notion.com/p/x)\n\
             Read http://x.y,,,,,\n"
                .into(),
        ),
        (
            format!("Journal/@May 2, 2023 {}.html", id(ENTRY)),
            page(
                ENTRY,
                "🤖",
                "<time datetime=\"2023-05-02\" data-notion-date-format=\"relative\">@May 2, 2023</time> ",
                &entry_props,
                &entry_body,
            ),
        ),
        (
            format!("Journal/Read x y {}.html", id(READING)),
            page(
                READING,
                "",
                "Read <a href=\"http://x.y\">x.y</a>",
                "",
                "<p>Notes</p>",
            ),
        ),
        (
            format!("Journal/Week Template {}.html", id(TEMPLATE)),
            page(TEMPLATE, "", "Week Template", "", "<p>Template</p>"),
        ),
        (
            format!("Journal/@May 2, 2023/Today {}.csv", id(TODAY)),
            "Name,Status,tag\nWrite tests,Done,work\nBuy milk,Not started,work\nCall: Bob,In progress,work\n"
                .into(),
        ),
        (
            format!("Journal/@May 2, 2023/Today/Write tests {}.html", id(WRITE_TESTS)),
            task(
                WRITE_TESTS,
                "Write tests",
                status("Done", "green"),
                "<pre class=\"code\"><code class=\"language-go\">go test</code></pre>",
            ),
        ),
        (
            format!("Journal/@May 2, 2023/Today/Buy milk {}.html", id(BUY_MILK)),
            task(BUY_MILK, "Buy milk", status("Not started", ""), "<p></p>"),
        ),
        (
            format!("Journal/@May 2, 2023/Today/Call Bob {}.html", id(CALL_BOB)),
            task(CALL_BOB, "Call: Bob", status("In progress", "blue"), ""),
        ),
        ("Journal/@May 2, 2023/photo.png".into(), b"PNG".to_vec()),
    ];
    // Zips made on macOS hold decomposed accents; the reader composes them.
    f.push((
        "Journal/@May 2, 2023/Cafe\u{301}.txt".into(),
        b"menu".to_vec(),
    ));
    f
}

/// A page with nothing but a subpage, which shows a database of rows
/// without more to say; and a database only a linked view brought along.
fn trips() -> Files {
    vec![
        (
            format!("Trips {}.html", id(TRIPS)),
            page(
                TRIPS,
                "✈️",
                "Trips",
                "",
                &format!(
                    "<figure class=\"link-to-page\">{}</figure><p></p>",
                    link(
                        MEXICO,
                        &format!("Trips/Mexico%20{}.html", id(MEXICO)),
                        "Mexico"
                    )
                ),
            ),
        ),
        (
            format!("Trips/Mexico {}.html", id(MEXICO)),
            page(
                MEXICO,
                "🇲🇽",
                "Mexico",
                "",
                &format!(
                    "<p>Back on {}.</p><div id=\"{}\" class=\"collection-content\">\
                     <h4 class=\"collection-title\">Schedule</h4>{}</div>",
                    link(ENTRY, "https://app.notion.com/p/x", "@May 2, 2023"),
                    uuid(SCHEDULE),
                    table(&["Name", "When"], &[(DAY_ONE, "x.html", "Day 1")])
                ),
            ),
        ),
        (
            format!("Trips/Mexico/Schedule {}.csv", id(SCHEDULE)),
            "Name,When\nDay 1,Morning\n".into(),
        ),
        (
            format!("Trips/Mexico/Schedule/Day 1 {}.html", id(DAY_ONE)),
            page(DAY_ONE, "", "Day 1", &prop("text", "When", "Morning"), ""),
        ),
        (
            format!("Tasks {}.csv", id(TASKS)),
            "Name,Done\nOld task,Yes\n".into(),
        ),
        (
            format!("Tasks/Old task {}.html", id(OLD_TASK)),
            page(
                OLD_TASK,
                "",
                "Old task",
                &prop(
                    "checkbox",
                    "Done",
                    "<div class=\"checkbox checkbox-on\"></div>",
                ),
                "",
            ),
        ),
    ]
}

fn opts() -> Options {
    Options {
        into: "Notion".into(),
        ..Options::default()
    }
}

fn file<'a>(c: &'a Converted, path: &str) -> &'a str {
    let (_, data) = c.files.iter().find(|(p, _)| p == path).unwrap_or_else(|| {
        let all: Vec<&String> = c.files.iter().map(|(p, _)| p).collect();
        panic!("no {path} in {all:#?}")
    });
    std::str::from_utf8(data).unwrap()
}

fn paths(c: &Converted) -> Vec<&str> {
    c.files.iter().map(|(p, _)| p.as_str()).collect()
}

#[test]
fn a_database_and_its_rows() {
    let c = convert(vec![journal()], &opts()).unwrap();
    assert_eq!(
        paths(&c),
        [
            "Notion/Imported from Notion.md",
            "Notion/Journal/2023-05-02/2023-05-02.md",
            "Notion/Journal/2023-05-02/Write tests.md",
            "Notion/Journal/Journal.md",
            "Notion/Journal/Read x.y.md",
            "Notion/attachments/Café.txt",
            "Notion/attachments/photo.png",
        ]
    );
    let index = file(&c, "Notion/Journal/Journal.md");
    assert!(index.starts_with(&format!("---\nnotion: {}\n---\n", id(JOURNAL))));
    assert!(index.contains("| Name | Tags | Created |\n|---|---|---|\n"));
    assert!(index.contains("| [[2023-05-02]] | Daily OPS, Work | @2023-05-01 20:52 |"));
    assert!(index.contains("| [[Read x.y]] |  |  |"), "{index}");
    let entry = file(&c, "Notion/Journal/2023-05-02/2023-05-02.md");
    let expected_front = format!(
        "---\nCreated: 2023-05-01T20:52\nDate: 2023-05-02\ntags:\n  - Daily-OPS\n  - Work\n\
         week: 18\nWeek Planning:\n  - \"[[Week 18]]\"\nnotion: {}\n---\n## To do\n",
        id(ENTRY)
    );
    assert!(entry.starts_with(&expected_front), "{entry}");
    assert_eq!(
        c.icons.get("Notion/Journal").map(String::as_str),
        Some("📓")
    );
    assert_eq!(
        c.icons
            .get("Notion/Journal/2023-05-02/2023-05-02.md")
            .map(String::as_str),
        Some("🤖")
    );
    // The template isn't imported; the report says so.
    assert!(!c.files.iter().any(|(p, _)| p.contains("Template")));
    assert_eq!(c.report.templates, ["Journal: Week Template"]);
    let rows: BTreeMap<&str, usize> = c
        .report
        .databases
        .iter()
        .map(|d| (d.title.as_str(), d.rows))
        .collect();
    assert_eq!(rows, BTreeMap::from([("Journal", 2), ("Today", 3)]));
}

#[test]
fn a_database_of_tasks_is_a_checklist() {
    let c = convert(vec![journal()], &opts()).unwrap();
    let entry = file(&c, "Notion/Journal/2023-05-02/2023-05-02.md");
    assert!(
        entry.contains(
            "**Today**\n\n- [x] [[Write tests]] · work\n- [ ] Buy milk · work\n\
             - [ ] Call: Bob · In progress · work\n"
        ),
        "{entry}"
    );
    let task = file(&c, "Notion/Journal/2023-05-02/Write tests.md");
    assert_eq!(
        task,
        format!(
            "---\nStatus: Done\ntag: work\nnotion: {}\n---\n```go\ngo test\n```\n",
            id(WRITE_TESTS)
        )
    );
    // A full-page database of tasks without more to say is one note.
    let c = convert(vec![trips()], &opts()).unwrap();
    assert_eq!(
        file(&c, "Notion/Tasks.md"),
        format!("---\nnotion: {}\n---\n- [x] Old task\n", id(TASKS))
    );
}

#[test]
fn blocks() {
    let c = convert(vec![journal()], &opts()).unwrap();
    let entry = file(&c, "Notion/Journal/2023-05-02/2023-05-02.md");
    for part in [
        "See [[Read x.y|the reading]] on @2023-05-03, **snake_case** and \\*stars\\*, [[Week 18]].",
        "<span style=\"color: #7c7c7c\">Gray <span style=\"background-color: #e5484d4d\">red</span></span>",
        "![[photo.png]]\n*A photo*",
        "> [!warning] 💡\n> Mind *this*\n>\n> Second",
        "<details>\n<summary>More &amp; less</summary>\n\nHidden\n\n</details>",
        "1. One\n2. Two\n   - Under",
        "- [x] Done thing",
        "````rust\nfn main() {}\n```\n````",
        "> Quoted\n> twice",
        "| A | B |\n|---|---|\n| 1 \\| 2 | x |",
        "[Example](https://example.com/x)",
        "Left\n\nRight",
        "$$\ne=mc^2\n$$",
        "---\n\n[[Journal]]\n",
    ] {
        assert!(entry.contains(part), "no\n{part}\nin\n{entry}");
    }
    let report = file(&c, &format!("Notion/{REPORT}"));
    for part in [
        "- Week 18: [[2023-05-02]]",
        "- Columns, shown one after the other: [[2023-05-02]]",
        "- Tables of contents: [[2023-05-02]]",
        "- Database templates, not imported: Journal: Week Template",
    ] {
        assert!(report.contains(part), "no\n{part}\nin\n{report}");
    }
}

#[test]
fn pages_folders_and_tables() {
    let c = convert(vec![trips()], &opts()).unwrap();
    assert_eq!(
        paths(&c),
        [
            "Notion/Imported from Notion.md",
            "Notion/Tasks.md",
            "Notion/Trips/Mexico.md",
        ]
    );
    // Trips has nothing but its subpage: a folder, with its icon.
    assert_eq!(c.icons.get("Notion/Trips").map(String::as_str), Some("✈️"));
    let mexico = file(&c, "Notion/Trips/Mexico.md");
    assert_eq!(
        mexico,
        format!(
            "---\nnotion: {}\n---\nBack on [[2023-05-02]].\n\n**Schedule**\n\n\
             | Name | When |\n|---|---|\n| Day 1 | Morning |\n",
            id(MEXICO)
        )
    );
    // The entry isn't in this export: its link waits for it.
    assert!(c.report.waiting.contains_key("2023-05-02"));
}

#[test]
fn pieces_imported_one_after_the_other() {
    let first = convert(vec![journal()], &opts()).unwrap();
    let existing: BTreeMap<String, String> = first
        .files
        .iter()
        .filter_map(|(p, d)| {
            let id = notekpr_notion::id_of(std::str::from_utf8(d).ok()?)?;
            Some((id, p.clone()))
        })
        .collect();
    let vault_paths: Vec<String> = first.files.iter().map(|(p, _)| p.clone()).collect();
    let next = Options {
        into: notekpr_notion::landing_folder(|n| n == "Notion"),
        existing,
        vault_paths,
        ..Options::default()
    };
    assert_eq!(next.into, "Notion 2");
    // The same piece again brings nothing new.
    let again = convert(vec![journal()], &next).unwrap();
    assert_eq!(paths(&again), ["Notion 2/Imported from Notion.md"]);
    assert_eq!(again.report.already, 4);
    // Another piece links to what came before.
    let later = convert(vec![trips()], &next).unwrap();
    assert!(file(&later, "Notion 2/Trips/Mexico.md").contains("Back on [[2023-05-02]]."));
    assert!(later.report.waiting.is_empty());
    // Both together resolve their links too.
    let both = convert(vec![journal(), trips()], &opts()).unwrap();
    assert!(file(&both, "Notion/Trips/Mexico.md").contains("Back on [[2023-05-02]]."));
}

#[test]
fn names_taken_elsewhere_in_the_vault_link_by_path() {
    let o = Options {
        vault_paths: vec!["Diary/2023-05-02.md".into(), "photo.png".into()],
        ..opts()
    };
    let c = convert(vec![journal(), trips()], &o).unwrap();
    let mexico = file(&c, "Notion/Trips/Mexico.md");
    // (A link to a day shows the day: no alias.)
    assert!(
        mexico.contains("Back on [[Notion/Journal/2023-05-02/2023-05-02]]."),
        "{mexico}"
    );
    let entry = file(&c, "Notion/Journal/2023-05-02/2023-05-02.md");
    assert!(entry.contains("![[Notion/attachments/photo.png]]"));
}

#[test]
fn left_out() {
    let o = Options {
        skip: [id(TODAY), id(READING)].into(),
        ..opts()
    };
    let c = convert(vec![journal()], &o).unwrap();
    assert!(
        !paths(&c)
            .iter()
            .any(|p| p.contains("Read") || p.contains("Write tests"))
    );
    assert!(file(&c, "Notion/Journal/Journal.md").contains("| Read x.y |"));
    // A link to a page left out waits for it, as for one not exported.
    // (Without Today's tasks, the entry has nothing inside: a note.)
    let entry = file(&c, "Notion/Journal/2023-05-02.md");
    assert!(entry.contains("See [[Read x.y|the reading]] on"), "{entry}");
    assert!(c.report.waiting.contains_key("Read x.y"));
}

#[test]
fn what_an_export_holds() {
    let export = notekpr_notion::Export::read(vec![journal(), trips()]).unwrap();
    let o = Options {
        existing: [(id(READING), "Notion/Journal/Read x.y.md".to_string())].into(),
        ..opts()
    };
    let items: Vec<(String, usize, Option<usize>, usize, bool)> = export
        .items(&o)
        .into_iter()
        .map(|i| (i.title, i.notes, i.rows, i.already, i.linked))
        .collect();
    // Journal: its index, the entry, the task with more to say, Read x.y.
    // Trips: Mexico only (Trips is a folder, Day 1 a row of a table).
    assert_eq!(
        items,
        [
            ("Journal".to_string(), 4, Some(2), 1, false),
            ("Tasks".to_string(), 1, Some(1), 0, true),
            ("Trips".to_string(), 1, None, 0, false),
        ]
    );
    let items = export.items(&o);
    assert_eq!(items[0].icon.as_deref(), Some("📓"));
    assert_eq!(items[0].id, id(JOURNAL));
}

#[test]
fn the_same_export_gives_the_same_files() {
    let a = convert(vec![journal(), trips()], &opts()).unwrap();
    let b = convert(vec![journal(), trips()], &opts()).unwrap();
    assert_eq!(a.files, b.files);
    assert_eq!(a.icons, b.icons);
}

#[test]
fn what_isnt_an_export() {
    let e = convert(vec![vec![("a.md".into(), b"# A".to_vec())]], &opts());
    assert_eq!(e.err(), Some(notekpr_notion::Error::Markdown));
    let e = convert(vec![vec![("a.txt".into(), b"A".to_vec())]], &opts());
    assert_eq!(e.err(), Some(notekpr_notion::Error::NotAnExport));
}
