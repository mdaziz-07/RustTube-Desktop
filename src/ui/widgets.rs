//! Shared page helpers.
use super::*;

pub enum Load<T> {
    Loading,
    Ready(T),
    Error(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(t) => Some(t),
            _ => None,
        }
    }
    pub fn ready_mut(&mut self) -> Option<&mut T> {
        match self {
            Load::Ready(t) => Some(t),
            _ => None,
        }
    }
}

/// Vertical scroll container used by every page, with a scrollbar that can be
/// dragged (or clicked on its track) and keyboard scrolling (see [`scroll_page_key`]).
pub fn page_scroll(id: &'static str, handle: &ScrollHandle) -> PageScroll {
    ACTIVE_PAGE.with(|a| *a.borrow_mut() = Some((id, handle.clone())));
    PageScroll {
        inner: div().id(id).size_full().overflow_y_scroll().track_scroll(handle),
        handle: handle.clone(),
    }
}

/// A page's scroll area. The scrollbar is added after the page's content so it
/// is drawn, and takes clicks, on top of it.
#[derive(IntoElement)]
pub struct PageScroll {
    inner: Stateful<Div>,
    handle: ScrollHandle,
}

impl ParentElement for PageScroll {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.inner.extend(elements)
    }
}

impl RenderOnce for PageScroll {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        use gpui_kit::component::scroll::{ScrollableElement as _, ScrollbarMode};
        // on hover rather than only while scrolling, so it can be found without a wheel
        let mode = if crate::store::settings_snapshot().always_scrollbars { ScrollbarMode::Always } else { ScrollbarMode::Hover };
        if gpui_kit::component::Theme::global(cx).scrollbar_mode != mode {
            gpui_kit::component::Theme::set_scrollbar_mode(mode, cx);
        }
        self.inner.vertical_scrollbar(&self.handle)
    }
}

thread_local! {
    /// the page scroll area drawn last, which is the page on screen
    static ACTIVE_PAGE: std::cell::RefCell<Option<(&'static str, ScrollHandle)>> = const { std::cell::RefCell::new(None) };
}

/// Scrolls the page on screen for PageUp/PageDown, Home/End, the arrow keys and
/// Space / Shift+Space. Returns false when the key isn't a scrolling key.
pub fn scroll_page_key(key: &str, shift: bool, window: &mut Window) -> bool {
    let Some((_id, handle)) = ACTIVE_PAGE.with(|a| a.borrow().clone()) else { return false };
    let view: f32 = handle.bounds().size.height.into();
    let max: f32 = handle.max_offset().y.into();
    let page = (view * 0.85).max(100.0);
    let delta = match (key, shift) {
        ("pagedown", _) | ("space", false) => -page,
        ("pageup", _) | ("space", true) => page,
        ("down", _) => -80.0,
        ("up", _) => 80.0,
        ("home", _) => f32::INFINITY,
        ("end", _) => f32::NEG_INFINITY,
        _ => return false,
    };
    let now: f32 = handle.offset().y.into();
    let target = (now + delta).clamp(-max.max(0.0), 0.0);
    handle.set_offset(point(handle.offset().x, px(target)));
    window.refresh();
    true
}

/// A scroll area inside a page (queue, chat, chapters). gpui hands a wheel event
/// to every scroll area under the pointer, so without this the page scrolls
/// along with (or instead of) the list the pointer is over.
pub fn nested_scroll(id: impl Into<ElementId>, list: impl IntoElement) -> Stateful<Div> {
    div().id(id).on_scroll(|_, _, cx| cx.stop_propagation()).child(list)
}

/// True when the scroll position is within `margin` of the bottom, or when the
/// page is laid out but its content doesn't fill it (nothing to scroll yet).
pub fn near_bottom(handle: &ScrollHandle, margin: f32) -> bool {
    let off: f32 = (-handle.offset().y).into();
    let max: f32 = handle.max_offset().y.into();
    let laid_out = handle.bounds().size.height > px(0.);
    (max > 0.0 && off >= max - margin) || (laid_out && max <= 0.0)
}

pub fn content_width(cx: &App) -> Pixels {
    let w = cx.global::<crate::ui::app::UiState>().content_width;
    if w <= px(100.) { px(1000.) } else { w }
}

pub fn section_title(text: impl Into<SharedString>, cx: &App) -> Div {
    let p = pal(cx);
    div().text_xl().font_weight(FontWeight::BOLD).text_color(p.text).child(text.into())
}

/// Sets up a "load more" trigger: call from render; spawns `f` when near the bottom.
pub fn maybe_load_more<T: 'static>(
    handle: &ScrollHandle,
    loading: &mut bool,
    has_more: bool,
    cx: &mut Context<T>,
    f: impl FnOnce(&mut T, &mut Context<T>) + 'static,
) {
    if has_more && !*loading && near_bottom(handle, 900.0) {
        *loading = true;
        cx.defer_in_entity(f);
    }
}

pub trait DeferExt<T> {
    fn defer_in_entity(&mut self, f: impl FnOnce(&mut T, &mut Context<T>) + 'static);
}

impl<T: 'static> DeferExt<T> for Context<'_, T> {
    fn defer_in_entity(&mut self, f: impl FnOnce(&mut T, &mut Context<T>) + 'static) {
        let weak = self.entity().downgrade();
        self.defer(move |cx| {
            if let Some(e) = weak.upgrade() {
                e.update(cx, f);
            }
        });
    }
}

// ───────────────────────── rich text (links, timestamps, hashtags) ─────────────────────────

#[derive(Clone)]
pub(crate) enum Token {
    Url(String),
    Time(f64),
    Hashtag(String),
    Handle(String),
}

pub(crate) fn tokenize(text: &str) -> Vec<(std::ops::Range<usize>, Token)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let at_word_start = i == 0 || !text[..i].chars().last().map(|c| c.is_alphanumeric()).unwrap_or(false);
        if at_word_start && (rest.starts_with("http://") || rest.starts_with("https://") || rest.starts_with("www.") || rest.starts_with("youtube.com/") || rest.starts_with("youtu.be/")) {
            let end = rest.find(|c: char| c.is_whitespace() || c == ')' || c == '"').unwrap_or(rest.len());
            let raw_url = rest[..end].trim_end_matches(['.', ',', '!', '?']).to_string();
            let url = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
                raw_url.clone()
            } else {
                format!("https://{raw_url}")
            };
            out.push((i..i + raw_url.len(), Token::Url(url)));
            i += raw_url.len().max(1);
            continue;
        }
        if at_word_start && bytes[i].is_ascii_digit() {
            let end = rest.find(|c: char| !(c.is_ascii_digit() || c == ':')).unwrap_or(rest.len());
            let cand = &rest[..end];
            if cand.contains(':') && !cand.ends_with(':') && cand.split(':').count() <= 3 && cand.split(':').skip(1).all(|p| p.len() == 2) {
                if let Some(secs) = crate::api::json::parse_duration(cand) {
                    out.push((i..i + end, Token::Time(secs as f64)));
                    i += end;
                    continue;
                }
            }
        }
        if at_word_start && (bytes[i] == b'#' || bytes[i] == b'@') && rest.len() > 1 {
            let end = rest[1..]
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.'))
                .map(|e| e + 1)
                .unwrap_or(rest.len());
            let word = rest[..end].trim_end_matches('.');
            if word.len() > 1 {
                let tok = if bytes[i] == b'#' { Token::Hashtag(word.to_string()) } else { Token::Handle(word.to_string()) };
                out.push((i..i + word.len(), tok));
                i += word.len();
                continue;
            }
        }
        if at_word_start && bytes[i] == b'/' && rest.len() > 1 {
            // Channel link in description/comments formatted as "/ channelname", "/channelname", "/@handle", etc.
            let after_slash = &rest[1..];
            let trimmed = after_slash.trim_start();
            let spaces = after_slash.len() - trimmed.len();
            let cand = trimmed.strip_prefix('@').unwrap_or(trimmed);
            let end = cand
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.'))
                .unwrap_or(cand.len());
            let name = cand[..end].trim_end_matches(['.', ',', '!', '?', ';', ':']);
            if name.len() >= 2 && name.chars().any(|c| c.is_alphabetic()) {
                let full_span_len = 1 + spaces + (trimmed.len() - cand.len()) + name.len();
                let handle = if trimmed.starts_with('@') { format!("@{name}") } else { name.to_string() };
                out.push((i..i + full_span_len, Token::Handle(handle)));
                i += full_span_len;
                continue;
            }
        }
        i += rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    out
}

/// Escapes text so Markdown shows it literally.
fn md_escape(t: &str, out: &mut String) {
    for c in t.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Plain text with links, timestamps, #hashtags and @handles as Markdown links
/// (app-internal schemes for the non-URL ones), line breaks kept.
fn to_markdown(text: &str) -> String {
    let mut md = String::with_capacity(text.len() + 64);
    let mut last = 0;
    for (r, tok) in tokenize(text) {
        md_escape(&text[last..r.start], &mut md);
        let label = &text[r.clone()];
        md.push('[');
        md_escape(label, &mut md);
        md.push_str("](");
        match tok {
            Token::Url(u) => {
                md.push('<');
                md.push_str(&u.replace(['<', '>', ' '], ""));
                md.push('>');
            }
            Token::Time(t) => md.push_str(&format!("st-time:{}", t as u64)),
            Token::Hashtag(h) => md.push_str(&format!("st-tag:{}", crate::api::http::urlencode(&h))),
            Token::Handle(h) => md.push_str(&format!("st-handle:{}", crate::api::http::urlencode(&h))),
        }
        md.push(')');
        last = r.end;
    }
    md_escape(&text[last..], &mut md);
    // single newlines are hard breaks, not spaces
    md.replace("\r\n", "\n").replace('\n', "  \n")
}

fn open_link(url: &str, window: &mut Window, cx: &mut App) {
    if let Some(t) = url.strip_prefix("st-time:") {
        let t: f64 = t.parse().unwrap_or(0.0);
        crate::player::controller::player(cx).update(cx, |p, cx| {
            p.seek_to(t);
            p.set_paused(false);
            cx.notify();
        });
        // back up to the player, like YouTube does
        scroll_page_key("home", false, window);
    } else if let Some(h) = url.strip_prefix("st-tag:") {
        crate::ui::app::navigate(crate::ui::app::Route::Search(crate::api::http::urldecode(h)), window, cx);
    } else if let Some(h) = url.strip_prefix("st-handle:") {
        let raw = crate::api::http::urldecode(h);
        let clean = raw.trim_start_matches('@').trim_start_matches('/').trim().to_string();
        cx.spawn(async move |cx| {
            let url1 = format!("https://www.youtube.com/@{clean}");
            let url2 = format!("https://www.youtube.com/{clean}");
            let res = blocking(move || {
                crate::api::innertube::resolve_url(&url1)
                    .or_else(|_| crate::api::innertube::resolve_url(&url2))
            }).await;
            if let Ok(crate::api::innertube::ResolvedUrl::Channel(id)) = res {
                let _ = cx.update(|cx| {
                    if let Some(w) = cx.active_window() {
                        let _ = w.update(cx, |_, window, cx| crate::ui::app::navigate(crate::ui::app::Route::Channel(id), window, cx));
                    }
                });
            }
        })
        .detach();
    } else if let Some(r) = crate::api::innertube::parse_youtube_url(url) {
        let route = match r {
            crate::api::innertube::ResolvedUrl::Video(id) => crate::ui::app::Route::Watch { id, playlist: None, start: None },
            crate::api::innertube::ResolvedUrl::Channel(id) => crate::ui::app::Route::Channel(id),
            crate::api::innertube::ResolvedUrl::Playlist(id) => crate::ui::app::Route::Playlist(id),
        };
        crate::ui::app::navigate(route, window, cx);
    } else {
        crate::ui::open_web(url);
    }
}

/// Selectable text (drag to select, Ctrl+C) with clickable links, timestamps,
/// hashtags and handles. Right-click copies the whole text.
pub fn rich_text(id: impl Into<ElementId>, text: String, cx: &App) -> AnyElement {
    rich_text_with(id, text, &[], cx)
}

/// [`rich_text`] with channel emojis (shortcut, image URL) drawn inline as images.
pub fn rich_text_with(id: impl Into<ElementId>, text: String, emojis: &[(String, String)], cx: &App) -> AnyElement {
    let p = pal(cx);
    let urls: Vec<String> = tokenize(&text).into_iter().filter_map(|(_, t)| if let Token::Url(u) = t { Some(u) } else { None }).collect();
    let md = if emojis.is_empty() {
        to_markdown(&text)
    } else {
        // swap each shortcut for a marker the Markdown conversion leaves alone,
        // then the marker for an inline image
        let mut marked = text.clone();
        for (i, (code, _)) in emojis.iter().enumerate() {
            marked = marked.replace(code.as_str(), &format!("\u{E000}{i}\u{E001}"));
        }
        let mut md = to_markdown(&marked);
        for (i, (code, url)) in emojis.iter().enumerate() {
            let alt: String = code.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
            let src: String = url.chars().filter(|c| !matches!(c, '<' | '>' | '"' | ' ')).collect();
            md = md.replace(&format!("\u{E000}{i}\u{E001}"), &format!(r#"<img src="{src}" alt="{alt}" width="18" height="18" />"#));
        }
        md
    };
    let view = gpui_kit::base::TextView::markdown(id, md)
        .selectable(true)
        .on_link_click(|url, _, window, cx| open_link(url, window, cx));
    let copy_text = text.clone();
    let _ = p;
    div()
        .w_full()
        .child(view)
        .on_mouse_down(MouseButton::Right, move |ev: &MouseDownEvent, window, cx| {
            cx.stop_propagation();
            let t = copy_text.clone();
            let mut items = vec![crate::ui::app::MenuItem::new("copy", "Copy all text", move |_, cx| crate::ui::copy_to_clipboard(t.clone(), cx))];
            for u in urls.iter().take(3) {
                let u2 = u.clone();
                let label = if u.chars().count() > 40 { format!("Copy link {}…", u.chars().take(40).collect::<String>()) } else { format!("Copy link {u}") };
                items.push(crate::ui::app::MenuItem::new("link", label, move |_, cx| crate::ui::copy_to_clipboard(u2.clone(), cx)));
            }
            crate::ui::app::open_menu(ev.position, items, window, cx);
        })
        .into_any_element()
}

thread_local! {
    /// long comments the user opened with "Read more"
    static OPEN_COMMENTS: std::cell::RefCell<std::collections::HashSet<SharedString>> = Default::default();
}

const CLAMP_LINES: usize = 5;
const CLAMP_CHARS: usize = 400;

/// The start of a long text (whole lines, then cut at a word), or None when it's short.
pub(crate) fn clamp_text(text: &str, max_lines: usize, max_chars: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = lines.iter().take(max_lines).copied().collect::<Vec<_>>().join("
");
    let mut cut = lines.len() > max_lines;
    if out.chars().count() > max_chars {
        let end = out.char_indices().nth(max_chars).map(|(i, _)| i).unwrap_or(out.len());
        let end = out[..end].rfind(char::is_whitespace).filter(|&i| i > end / 2).unwrap_or(end);
        out.truncate(end);
        cut = true;
    }
    cut.then(|| format!("{}…", out.trim_end()))
}

/// A comment's text (with channel emojis) and the GIF or sticker posted with it.
/// Long ones are cut to a few lines with "Read more".
pub fn comment_body(id: impl Into<SharedString>, c: &crate::api::types::Comment, cx: &App) -> AnyElement {
    let id: SharedString = id.into();
    let p = pal(cx);
    let open = OPEN_COMMENTS.with(|o| o.borrow().contains(&id));
    let short = clamp_text(&c.text, CLAMP_LINES, CLAMP_CHARS);
    let long = short.is_some();
    let text = if open { c.text.clone() } else { short.unwrap_or_else(|| c.text.clone()) };
    let toggle_id = id.clone();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(rich_text_with(id.clone(), text, &c.emojis, cx))
        .when(long, |d| {
            d.child(
                div()
                    .id(SharedString::from(format!("{id}-more")))
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text2)
                    .cursor_pointer()
                    .hover(move |s| s.text_color(p.text))
                    .child(if open { "Show less" } else { "Read more" })
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        OPEN_COMMENTS.with(|o| {
                            let mut o = o.borrow_mut();
                            if !o.remove(&toggle_id) {
                                o.insert(toggle_id.clone());
                            }
                        });
                        window.refresh();
                    }),
            )
        })
        .when_some(c.image.clone(), |d, (url, ratio)| {
            // at most 200px tall and 240px wide, aspect kept
            let h = (240.0 / ratio).min(200.0);
            let w = h * ratio;
            // the id gives the image the per-element state gpui keeps the
            // animation frame in; without one a GIF only ever shows its first frame
            d.child(img(SharedUri::from(url)).id(SharedString::from(format!("{id}-img"))).w(px(w)).h(px(h)).rounded_lg().object_fit(ObjectFit::Contain))
        })
        .into_any_element()
}

#[cfg(test)]
mod clamp_tests {
    use super::clamp_text;
    #[test]
    fn clamps() {
        assert_eq!(clamp_text("short", 5, 400), None);
        assert_eq!(clamp_text("a
b
c", 2, 400).as_deref(), Some("a
b…"));
        let long = "word ".repeat(200);
        let c = clamp_text(&long, 5, 400).unwrap();
        assert!(c.chars().count() <= 401 && c.ends_with("word…"));
        // multi-byte text is cut on a char boundary
        assert!(clamp_text(&"é".repeat(500), 5, 400).is_some());
    }
}

/// First timestamp ("1:23", "1:02:03") mentioned in a text, in seconds.
pub fn first_timestamp(text: &str) -> Option<f64> {
    tokenize(text).into_iter().find_map(|(_, t)| if let Token::Time(s) = t { Some(s) } else { None })
}
