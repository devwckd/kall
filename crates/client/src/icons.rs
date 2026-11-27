//! A handful of 24×24 stroke icons drawn for this POC.

use rune::Icon;

macro_rules! stroke_icon {
    ($id:ident, $name:literal, $body:literal) => {
        pub static $id: Icon = Icon {
            name: $name,
            view: 24.0,
            body: concat!(
                r#"<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#,
                $body,
                "</g>"
            ),
        };
    };
}

stroke_icon!(HASH, "kall-hash", r#"<path d="M5 9h15M4 15h15M10 3 8 21M16 3l-2 18"/>"#);
stroke_icon!(SPEAKER, "kall-speaker", r#"<path d="M11 5 6 9H3v6h3l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7M19 5a10 10 0 0 1 0 14"/>"#);
stroke_icon!(MIC, "kall-mic", r#"<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>"#);
stroke_icon!(MIC_OFF, "kall-mic-off", r#"<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3M3 3l18 18"/>"#);
stroke_icon!(HEADPHONES, "kall-headphones", r#"<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="4" height="7" rx="1.5"/><rect x="17" y="14" width="4" height="7" rx="1.5"/>"#);
stroke_icon!(HEADPHONES_OFF, "kall-headphones-off", r#"<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="4" height="7" rx="1.5"/><rect x="17" y="14" width="4" height="7" rx="1.5"/><path d="M3 3l18 18"/>"#);
/// A handset turned on its back: the "hang up" button.
pub static PHONE_OFF: Icon = Icon {
    name: "kall-phone-off",
    view: 24.0,
    body: r#"<path fill="currentColor" d="M12 8.5c-3.6 0-6.9 1.2-9.3 3.3-.5.4-.7 1-.7 1.6v2.2c0 .9.8 1.5 1.6 1.3l3.4-.8c.6-.1 1-.7 1-1.3v-2.1a11 11 0 0 1 8 0v2.1c0 .6.4 1.2 1 1.3l3.4.8c.8.2 1.6-.4 1.6-1.3v-2.2c0-.6-.2-1.2-.7-1.6-2.4-2.1-5.7-3.3-9.3-3.3z"/>"#,
};
/// The kall ghost: one shape, eyes cut out (`docs/logo-white.svg`).
pub static GHOST: Icon = Icon {
    name: "kall-ghost",
    view: 96.0,
    body: r#"<path fill="currentColor" fill-rule="evenodd" d="M20 46A28 28 0 0 1 76 46V78q-4.667 8-9.333 0q-4.667-8-9.334 0q-4.666 8-9.333 0q-4.667-8-9.333 0q-4.667 8-9.334 0q-4.666-8-9.333 0z M33 46a5 7 0 1 0 10 0a5 7 0 1 0-10 0z M53 46a5 7 0 1 0 10 0a5 7 0 1 0-10 0z"/>"#,
};
stroke_icon!(SEND, "kall-send", r#"<path d="M22 2 11 13M22 2l-7 20-4-9-9-4z"/>"#);
stroke_icon!(GEAR, "kall-gear", r#"<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>"#);
stroke_icon!(REPLY, "kall-reply", r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>"#);
stroke_icon!(PENCIL, "kall-pencil", r#"<path d="M17 3a2.8 2.8 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5z"/>"#);
stroke_icon!(TRASH, "kall-trash", r#"<path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/>"#);
stroke_icon!(X, "kall-x", r#"<path d="M18 6 6 18M6 6l12 12"/>"#);
stroke_icon!(UP, "kall-up", r#"<path d="m18 15-6-6-6 6"/>"#);
stroke_icon!(DOWN, "kall-down", r#"<path d="m6 9 6 6 6-6"/>"#);
stroke_icon!(SCREEN, "kall-screen", r#"<rect x="2" y="4" width="20" height="13" rx="2"/><path d="M8 21h8M12 17v4"/>"#);
stroke_icon!(CAMERA, "kall-camera", r#"<path d="M15 10l5-3v10l-5-3z"/><rect x="2" y="6" width="13" height="12" rx="2"/>"#);
stroke_icon!(WINDOW, "kall-window", r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M7 6.5h.01M10 6.5h.01"/>"#);
stroke_icon!(BELL, "kall-bell", r#"<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/>"#);
stroke_icon!(BELL_OFF, "kall-bell-off", r#"<path d="M8.7 3A6 6 0 0 1 18 8a21.3 21.3 0 0 0 .6 5M17 17H3s3-2 3-9a4.67 4.67 0 0 1 .3-1.7M10.3 21a1.94 1.94 0 0 0 3.4 0M2 2l20 20"/>"#);
