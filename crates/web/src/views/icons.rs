//! Inline SVG icons (stroke-based, 24×24).

use leptos::prelude::*;

fn paths(name: &str) -> &'static str {
    match name {
        "plus" => r#"<path d="M12 5v14M5 12h14"/>"#,
        "search" => r#"<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>"#,
        "folder" => {
            r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#
        }
        "folder-plus" => {
            r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M12 11v5M9.5 13.5h5"/>"#
        }
        "file" => {
            r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>"#
        }
        "file-plus" => {
            r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M12 11v6M9 14h6"/>"#
        }
        "drawing" => {
            r#"<rect x="3" y="3" width="8" height="8" rx="1.5"/><circle cx="17" cy="7" r="4"/><path d="M3 18c2.5-3 5 3 7.5 0s5 3 7.5 0 2.5 1.5 3 1"/>"#
        }
        "paperclip" => {
            r#"<path d="m21 11-8.5 8.5a5 5 0 0 1-7-7L14 4a3.5 3.5 0 0 1 5 5l-8.5 8.5a2 2 0 0 1-3-3L15 7"/>"#
        }
        "chevron-right" => r#"<path d="m9 6 6 6-6 6"/>"#,
        "chevron-down" => r#"<path d="m6 9 6 6 6-6"/>"#,
        "star" => {
            r#"<path d="m12 3 2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9z"/>"#
        }
        "clock" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>"#,
        "at" => {
            r#"<circle cx="12" cy="12" r="4"/><path d="M16 8v5a3 3 0 0 0 6 0v-1a10 10 0 1 0-4 8"/>"#
        }
        "bell" => {
            r#"<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.9 1.9 0 0 0 3.4 0"/>"#
        }
        "trash" => r#"<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>"#,
        "settings" => {
            r#"<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>"#
        }
        "share" => {
            r#"<circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><path d="m8.6 13.5 6.8 4M15.4 6.5l-6.8 4"/>"#
        }
        "history" => r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5M12 7v5l4 2"/>"#,
        "more" => {
            r#"<circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/>"#
        }
        "lock" => {
            r#"<rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>"#
        }
        "users" => {
            r#"<circle cx="9" cy="8" r="4"/><path d="M2 21a7 7 0 0 1 14 0M16 3.1a4 4 0 0 1 0 7.8M22 21a7 7 0 0 0-5-6.7"/>"#
        }
        "eye" => {
            r#"<path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>"#
        }
        "edit" => r#"<path d="M12 20h9M16.5 3.5a2.1 2.1 0 1 1 3 3L7 19l-4 1 1-4z"/>"#,
        "code" => r#"<path d="m16 18 6-6-6-6M8 6l-6 6 6 6"/>"#,
        "code-block" => {
            r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="m10 10-2 2 2 2M14 10l2 2-2 2"/>"#
        }
        "book" => {
            r#"<path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20V3H6.5A2.5 2.5 0 0 0 4 5.5z"/><path d="M4 19.5A2.5 2.5 0 0 0 6.5 22H20v-5"/>"#
        }
        "check" => r#"<path d="m5 12 5 5L20 7"/>"#,
        "x" => r#"<path d="M18 6 6 18M6 6l12 12"/>"#,
        "menu" => r#"<path d="M4 6h16M4 12h16M4 18h16"/>"#,
        "panel" => r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/>"#,
        "link" => {
            r#"<path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7"/><path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7"/>"#
        }
        "image" => {
            r#"<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-5-5L5 21"/>"#
        }
        "upload" => {
            r#"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M17 8l-5-5-5 5M12 3v12"/>"#
        }
        "download" => {
            r#"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/>"#
        }
        "logout" => r#"<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9"/>"#,
        "device" => {
            r#"<rect x="2" y="4" width="20" height="13" rx="2"/><path d="M8 21h8M12 17v4"/>"#
        }
        "key" => {
            r#"<circle cx="7.5" cy="15.5" r="4.5"/><path d="m10.7 12.3 9.3-9.3M15 7l3 3M18 4l2 2"/>"#
        }
        "bot" => {
            r#"<rect x="3" y="8" width="18" height="12" rx="2"/><path d="M12 8V4M8 4h8M9 14h.01M15 14h.01"/>"#
        }
        "shield" => r#"<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/>"#,
        "bold" => r#"<path d="M7 5h6a3.5 3.5 0 0 1 0 7H7zM7 12h7a3.5 3.5 0 0 1 0 7H7z"/>"#,
        "italic" => r#"<path d="M19 4h-9M14 20H5M15 4 9 20"/>"#,
        "strike" => r#"<path d="M16 4H9a3 3 0 0 0-2.8 4M14 12a4 4 0 0 1 0 8H6M4 12h16"/>"#,
        "heading" => r#"<path d="M6 4v16M18 4v16M6 12h12"/>"#,
        "text-color" => r#"<path d="m7 15 5-11 5 11M8.8 11h6.4M4 20h16"/>"#,
        "list" => r#"<path d="M9 6h11M9 12h11M9 18h11M4.5 6h.01M4.5 12h.01M4.5 18h.01"/>"#,
        "list-ordered" => {
            r#"<path d="M10 6h11M10 12h11M10 18h11M4 6h1v4M4 10h2M6 18H4c0-1 2-2 2-3s-1-1.5-2-1"/>"#
        }
        "checkbox" => {
            r#"<rect x="3" y="3" width="18" height="18" rx="3"/><path d="m8 12 3 3 5-6"/>"#
        }
        "quote" => r#"<path d="M6 17h3l2-4V7H5v6h3zM15 17h3l2-4V7h-6v6h3z"/>"#,
        "callout" => {
            r#"<rect x="3" y="4" width="18" height="16" rx="3"/><path d="M12 11v5M12 8v.01"/>"#
        }
        "table" => {
            r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 10h18M3 15h18M9 4v16M15 4v16"/>"#
        }
        "minus" => r#"<path d="M5 12h14"/>"#,
        "undo" => r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 0 10h-3"/>"#,
        "redo" => r#"<path d="m15 14 5-5-5-5"/><path d="M20 9H9a5 5 0 0 0 0 10h3"/>"#,
        "cloud" => r#"<path d="M17.5 19H8a6 6 0 1 1 5.7-7.9A4.5 4.5 0 1 1 17.5 19z"/>"#,
        "cloud-off" => {
            r#"<path d="m2 2 20 20M5.8 8.2A6 6 0 0 0 8 19h9.5M21 15.9A4.5 4.5 0 0 0 15.4 9"/>"#
        }
        "refresh" => {
            r#"<path d="M21 12a9 9 0 0 1-15.5 6.3L3 16M3 12a9 9 0 0 1 15.5-6.3L21 8M21 3v5h-5M3 21v-5h5"/>"#
        }
        "archive" => {
            r#"<rect x="3" y="4" width="18" height="5" rx="1"/><path d="M5 9v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V9M10 13h4"/>"#
        }
        "tag" => {
            r#"<path d="M20.6 13.4 13.4 20.6a2 2 0 0 1-2.8 0L3 13V3h10l7.6 7.6a2 2 0 0 1 0 2.8z"/><circle cx="7.5" cy="7.5" r="1.5"/>"#
        }
        "hash" => r#"<path d="M4 9h16M4 15h16M10 3 8 21M16 3l-2 18"/>"#,
        "vault" => {
            r#"<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="12" cy="12" r="4"/><path d="M12 8v1M12 15v1M8 12h1M15 12h1"/>"#
        }
        "keyboard" => {
            r#"<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>"#
        }
        "alert" => {
            r#"<path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0zM12 9v4M12 17h.01"/>"#
        }
        "copy" => {
            r#"<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>"#
        }
        "sun" => {
            r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>"#
        }
        "moon" => r#"<path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z"/>"#,
        "move" => {
            r#"<path d="M5 9l-3 3 3 3M9 5l3-3 3 3M15 19l-3 3-3-3M19 9l3 3-3 3M2 12h20M12 2v20"/>"#
        }
        "zap" => r#"<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>"#,
        "graph" => {
            r#"<circle cx="6" cy="6" r="2.5"/><circle cx="18" cy="8" r="2.5"/><circle cx="9" cy="18" r="2.5"/><path d="M8.3 7.1 15.6 7.7M7 8.4l1.4 7.2M10.8 16.3l5.6-6.2"/>"#
        }
        "maximize" => r#"<path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/>"#,
        "sort" => r#"<path d="M7 4v16M3.5 16.5 7 20l3.5-3.5M13 6h8M13 11h6M13 16h4"/>"#,
        "comment" => r#"<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1.2-4.1A8 8 0 1 1 21 12z"/>"#,
        "smile" => {
            r#"<circle cx="12" cy="12" r="9"/><path d="M8.5 14.5a4.5 4.5 0 0 0 7 0M9 9.5h.01M15 9.5h.01"/>"#
        }
        _ => r#"<circle cx="12" cy="12" r="2"/>"#,
    }
}

#[component]
pub fn Icon(#[prop(into)] name: String, #[prop(optional, into)] class: String) -> impl IntoView {
    let class = if class.is_empty() {
        "ic".to_string()
    } else {
        format!("ic {class}")
    };
    view! {
        <svg class=class viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" inner_html=paths(&name)></svg>
    }
}
