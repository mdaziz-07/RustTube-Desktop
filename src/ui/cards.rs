//! Video / channel / playlist cards in YouTube desktop style.
use super::*;
use crate::i18n::{t, tf};
use crate::api::types::*;
use crate::store;
use crate::ui::app::{MenuItem, Route, navigate, open_in_new_tab, open_menu};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

// ───────────────────────── watched progress cache ─────────────────────────

#[derive(Default)]
pub struct Watched(pub HashMap<String, (f64, f64)>);
impl Global for Watched {}

pub fn watched_progress(id: &str, cx: &App) -> Option<f32> {
    let (p, d) = *cx.try_global::<Watched>()?.0.get(id)?;
    if d > 0.0 {
        Some((p / d).clamp(0.0, 1.0) as f32)
    } else {
        Some(0.0)
    }
}

pub fn is_watched(id: &str, cx: &App) -> bool {
    cx.try_global::<Watched>().map(|w| w.0.contains_key(id)).unwrap_or(false)
}

// ───────────────────────── DeArrow lazy lookups ─────────────────────────

fn pending() -> &'static Mutex<HashSet<String>> {
    static P: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

/// Returns (title, thumbnail) possibly replaced by DeArrow.
pub fn branded(v: &VideoItem, cx: &mut App) -> (String, String) {
    let s = store::settings_snapshot();
    if !s.dearrow_titles && !s.dearrow_thumbnails {
        return (v.title.clone(), v.thumbnail.clone());
    }
    match crate::api::services::branding_cached(&v.id) {
        Some(Some(b)) => {
            let title = if s.dearrow_titles { b.title.clone().unwrap_or(v.title.clone()) } else { v.title.clone() };
            let thumb = match (s.dearrow_thumbnails, b.thumbnail_time) {
                (true, Some(t)) => crate::api::services::dearrow_thumbnail_url(&v.id, t),
                _ => v.thumbnail.clone(),
            };
            (title, thumb)
        }
        Some(None) => (v.title.clone(), v.thumbnail.clone()),
        None => {
            let id = v.id.clone();
            if pending().lock().insert(id.clone()) {
                cx.spawn(async move |cx| {
                    let id2 = id.clone();
                    blocking(move || crate::api::services::dearrow_branding(&id2)).await;
                    // cached now; dropping the mark lets it be looked up again if the cache is trimmed
                    pending().lock().remove(&id);
                    let _ = cx.update(|cx| cx.refresh_windows());
                })
                .detach();
            }
            (v.title.clone(), v.thumbnail.clone())
        }
    }
}

// ───────────────────────── shared pieces ─────────────────────────

pub fn meta_line(v: &VideoItem) -> String {
    let s = store::settings_snapshot();
    let mut parts = Vec::new();
    if !s.hide_view_counts {
        if let Some(vw) = &v.views_text {
            parts.push(vw.clone());
        }
    }
    if v.is_live {
        if parts.is_empty() {
            parts.push(t("LIVE").into());
        }
    } else if let Some(p) = &v.published_text {
        parts.push(p.clone());
    } else if let Some(ts) = v.published_ts {
        parts.push(crate::api::json::relative_from_ts(ts));
    }
    parts.join(", ")
}

fn badge(v: &VideoItem, cx: &App) -> Option<AnyElement> {
    let _ = cx;
    let (text, bg): (String, Hsla) = if v.is_live {
        (t("LIVE").into(), rgb(0xcc0000).into())
    } else if v.is_upcoming {
        (t("UPCOMING").into(), hsla(0., 0., 0., 0.8))
    } else if v.is_short && v.duration_text.is_none() {
        ("SHORTS".into(), hsla(0., 0., 0., 0.8))
    } else {
        (v.duration_text.clone()?, hsla(0., 0., 0., 0.8))
    };
    Some(
        div()
            .absolute()
            .bottom_1()
            .right_1()
            .px_1()
            .rounded_sm()
            .bg(bg)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(white())
            .child(text)
            .into_any_element(),
    )
}

pub fn thumbnail(v: &VideoItem, url: String, w: Pixels, h: Pixels, radius: Pixels, cx: &App) -> Div {
    thumbnail_with_bounds(v, url, w, h, radius, None, cx)
}

fn thumbnail_with_bounds(
    v: &VideoItem,
    url: String,
    w: Pixels,
    h: Pixels,
    radius: Pixels,
    slot_bounds: Option<std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>>,
    cx: &App,
) -> Div {
    let p = pal(cx);
    let progress = watched_progress(&v.id, cx);
    let r = radius * (store::settings_snapshot().thumb_radius / 12.0);
    div()
        .relative()
        .w(w)
        .h(h)
        .flex_none()
        .rounded(r)
        .overflow_hidden()
        .bg(p.surface)
        // the card's ratio: otherwise the image's own (4:3 for letterboxed
        // sddefault/hqdefault) wins over the size, the image ends up taller than
        // the card and shows top-aligned with the bottom cut off
        .child(img(SharedString::from(url)).size_full().rounded(r).aspect_ratio(w / h).object_fit(ObjectFit::Cover))
        .child(crate::player::preview::slot_marker(&v.id, slot_bounds, cx))
        .children(badge(v, cx))
        .when_some(progress.filter(|p| *p > 0.01), |d, pr| {
            d.child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(px(4.))
                    .bg(hsla(0., 0., 0.6, 0.6))
                    .child(div().h_full().w(relative(pr)).bg(p.accent)),
            )
        })
}

pub fn video_menu_items(v: &VideoItem) -> Vec<MenuItem> {
    let mut items = Vec::new();
    {
        let route = if v.is_short { Route::Shorts(Some(v.id.clone())) } else { Route::Watch { id: v.id.clone(), playlist: None, start: None } };
        items.push(MenuItem::new("app-window", t("Open in new tab"), move |window, cx| {
            crate::ui::app::open_in_new_tab(route.clone(), false, window, cx)
        }));
    }
    let v1 = v.clone();
    items.push(MenuItem::new("list-plus", t("Add to queue"), move |_, cx| {
        crate::player::controller::enqueue(v1.clone(), cx);
        toast(cx, t("Added to queue"));
    }));
    let v2 = v.clone();
    items.push(MenuItem::new("clock", t("Save to Watch later"), move |_, cx| {
        if let Some(wl) = store::playlists().into_iter().find(|p| p.protected && p.name == "Watch later") {
            store::playlist_add(wl.id, &v2);
            toast(cx, t("Saved to Watch later"));
        }
    }));
    let v3 = v.clone();
    items.push(MenuItem::new("bookmark", t("Save to playlist…"), move |window, cx| {
        crate::ui::app::open_save_dialog(v3.clone(), window, cx);
    }));
    let v4 = v.clone();
    items.push(MenuItem::new("download", t("Download"), move |_, cx| {
        crate::downloads::start(v4.clone(), None, cx);
    }));
    let v5 = v.clone();
    items.push(MenuItem::new("external-link", t("Open in external player"), move |_, cx| {
        crate::downloads::open_external(&v5.id, cx);
    }));
    let id = v.id.clone();
    items.push(MenuItem::new("link", t("Copy link"), move |_, cx| {
        copy_to_clipboard(format!("https://www.youtube.com/watch?v={id}"), cx);
    }));
    let id2 = v.id.clone();
    items.push(MenuItem::new("globe", t("Open on YouTube"), move |_, _| {
        let _ = open::that(format!("https://www.youtube.com/watch?v={id2}"));
    }));
    if let Some(cid) = v.channel_id.clone() {
        let name = v.channel_name.clone();
        items.push(MenuItem::new("ban", t("Don't recommend channel"), move |_, cx| {
            let (cid, name) = (cid.clone(), name.clone());
            store::update_settings(|s| {
                if !s.blocked_channels.iter().any(|(i, _)| *i == cid) {
                    s.blocked_channels.push((cid, name));
                }
            });
            toast(cx, t("Channel blocked. Manage in Settings → Content filters"));
            cx.refresh_windows();
        }));
    }
    if is_watched_in_history(&v.id) {
        let id3 = v.id.clone();
        items.push(MenuItem::new("eye-off", t("Remove from history"), move |_, cx| {
            store::history_remove(&id3);
            cx.default_global::<Watched>().0.remove(&id3);
            cx.refresh_windows();
        }));
    }
    items
}

fn is_watched_in_history(id: &str) -> bool {
    store::progress_map().contains_key(id)
}

fn more_button(id: SharedString, v: &VideoItem, cx: &App) -> Stateful<Div> {
    let p = pal(cx);
    let v = v.clone();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size_8()
        .rounded_full()
        .cursor_pointer()
        .hover(move |s| s.bg(p.hover))
        .child(icon("ellipsis-vertical").size_4().text_color(p.text))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |ev, window, cx| {
            cx.stop_propagation();
            open_menu(ev.position(), video_menu_items(&v), window, cx);
        })
}

fn watch_route(v: &VideoItem) -> Route {
    Route::Watch { id: v.id.clone(), playlist: None, start: None }
}

fn middle_click_tab(route: Route) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        if let ClickEvent::Mouse(click) = event {
            if click.down.button == MouseButton::Middle && click.up.button == MouseButton::Middle {
                cx.stop_propagation();
                open_in_new_tab(route.clone(), false, window, cx);
            }
        }
    }
}

// ───────────────────────── grid card (home / subscriptions / channel) ─────────────────────────

/// Starts resolving a video's streams when the pointer rests on its thumbnail,
/// so playback begins almost immediately on click.
thread_local! {
    static HOVER_FADES: std::cell::RefCell<std::collections::HashMap<String, (f32, std::time::Instant, bool)>> = Default::default();
}

fn set_card_hover(key: &str, hovered: bool, window: &mut Window) {
    let now = std::time::Instant::now();
    HOVER_FADES.with(|f| {
        let mut map = f.borrow_mut();
        if map.len() > 250 {
            map.retain(|_, (a, _, act)| *act || *a > 0.005);
        }
        let e = map.entry(key.to_string()).or_insert((0.0, now, hovered));
        e.1 = now;
        e.2 = hovered;
    });
    window.refresh();
}

fn hover_bg_plate(key: String, thumb_url: String, inset: Pixels, radius: Pixels, cx: &App) -> Div {
    let p = pal(cx);
    let fallback = hsla(0., 0., if p.dark { 0.72 } else { 0.45 }, if p.dark { 0.08 } else { 0.12 });
    let dark = p.dark;
    div()
        .absolute()
        .inset(inset)
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let now = std::time::Instant::now();
                    let (alpha, animating) = HOVER_FADES.with(|f| {
                        let mut map = f.borrow_mut();
                        let Some((alpha, last, active)) = map.get_mut(&key) else {
                            return (0.0f32, false);
                        };
                        let target = if *active { 1.0 } else { 0.0 };
                        let dt = now.duration_since(*last).as_secs_f32().min(0.05);
                        *last = now;
                        let gap = target - *alpha;
                        if gap.abs() <= 0.005 {
                            *alpha = target;
                            (*alpha, false)
                        } else {
                            *alpha += gap * (1.0 - (-dt / 0.08).exp());
                            (*alpha, true)
                        }
                    });
                    if animating {
                        window.request_animation_frame();
                    }
                    if alpha > 0.005 {
                        let mut color = crate::ui::image_cache::thumb_hover_bg(&thumb_url, dark).unwrap_or(fallback);
                        color.a *= alpha;
                        let mut quad = fill(bounds, color);
                        quad.corner_radii = Corners::all(radius);
                        window.paint_quad(quad);
                    }
                },
            )
            .size_full(),
        )
}

fn prefetch_hover(
    hover_key: String,
    id: &str,
    slot_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let id = id.to_string();
    move |hovered, window, cx| {
        set_card_hover(&hover_key, *hovered, window);
        if *hovered {
            crate::api::streams::prefetch_on_hover(&id);
        }
        crate::player::preview::hover(&id, *hovered, slot_bounds.get(), cx);
    }
}

fn glimpse_hover(
    hover_key: String,
    id: &str,
    slot_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let id = id.to_string();
    move |hovered, window, cx| {
        set_card_hover(&hover_key, *hovered, window);
        if *hovered {
            crate::api::streams::prefetch_on_hover(&id);
        }
        crate::player::preview::hover_glimpse(&id, *hovered, slot_bounds.get(), cx);
    }
}

pub fn video_card(v: &VideoItem, width: Pixels, show_channel: bool, cx: &mut App) -> AnyElement {
    if v.is_short {
        return shorts_card(v, width.min(px(240.)), cx);
    }
    let p = pal(cx);
    let (title, thumb) = branded(v, cx);
    let h = width * (9. / 16.);
    let route = watch_route(v);
    let channel_route = v.channel_id.clone().map(Route::Channel);
    let meta = meta_line(v);
    let show_avatar = show_channel && !store::settings_snapshot().hide_channel_avatars;
    let slot_bounds = std::rc::Rc::new(std::cell::Cell::new(None));
    let hover_key = format!("vc-{}", v.id);
    div()
        .id(SharedString::from(hover_key.clone()))
        .relative()
        .on_hover(prefetch_hover(hover_key.clone(), &v.id, slot_bounds.clone()))
        .flex()
        .flex_col()
        .w(width)
        .gap_3()
        .cursor_pointer()
        .group("card")
        .on_aux_click(middle_click_tab(route.clone()))
        .on_click(move |_, window, cx| {
            if !crate::player::preview::consumed_click(cx) {
                navigate(route.clone(), window, cx);
            }
        })
        .child(hover_bg_plate(hover_key, thumb.clone(), px(-8.), px(16.), cx))
        .child(thumbnail_with_bounds(v, thumb, width, h, px(12.), Some(slot_bounds), cx))
        .child(
            div()
                .flex()
                .gap_3()
                .w_full()
                .when(show_avatar, |d| {
                    let cr = channel_route.clone();
                    d.child(
                        div()
                            .id(SharedString::from(format!("va-{}", v.id)))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                if let Some(r) = cr.clone() {
                                    navigate(r, window, cx);
                                }
                            })
                            .child(avatar(v.channel_avatar.as_deref(), px(36.), &v.channel_name, cx)),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.text)
                                .line_clamp(2)
                                .child(title),
                        )
                        .when(show_channel && !v.channel_name.is_empty(), |d| {
                            let cr = channel_route.clone();
                            d.child(
                                div()
                                    .id(SharedString::from(format!("vn-{}", v.id)))
                                    .text_sm()
                                    .text_color(p.text2)
                                    .hover(move |s| s.text_color(p.text))
                                    .truncate()
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        if let Some(r) = cr.clone() {
                                            navigate(r, window, cx);
                                        }
                                    })
                                    .child(v.channel_name.clone()),
                            )
                        })
                        .child(div().text_sm().text_color(p.text2).truncate().child(meta)),
                )
                .child(more_button(SharedString::from(format!("vm-{}", v.id)), v, cx)),
        )
        .into_any_element()
}

pub fn shorts_card(v: &VideoItem, width: Pixels, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let (title, thumb) = branded(v, cx);
    let route = if store::settings_snapshot().shorts_in_player {
        watch_route(v)
    } else {
        Route::Shorts(Some(v.id.clone()))
    };
    let h = width * (16. / 9.);
    let hover_key = format!("sc-{}", v.id);
    let hk2 = hover_key.clone();
    div()
        .id(SharedString::from(hover_key.clone()))
        .relative()
        .on_hover(move |hovered, window, _| set_card_hover(&hk2, *hovered, window))
        .flex()
        .flex_col()
        .w(width)
        .gap_2()
        .cursor_pointer()
        .on_aux_click(middle_click_tab(route.clone()))
        .on_click(move |_, window, cx| navigate(route.clone(), window, cx))
        .child(hover_bg_plate(hover_key, thumb.clone(), px(-8.), px(16.), cx))
        .child(thumbnail(v, thumb, width, h, px(12.), cx))
        .child(
            div()
                .flex()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).line_clamp(2).child(title))
                        .when_some(v.views_text.clone(), |d, vt| d.child(div().text_xs().text_color(p.text2).child(vt))),
                )
                .child(more_button(SharedString::from(format!("sm-{}", v.id)), v, cx)),
        )
        .into_any_element()
}

// ───────────────────────── list row (search results) ─────────────────────────

pub fn video_row(v: &VideoItem, thumb_w: Pixels, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let (title, thumb) = branded(v, cx);
    let route = watch_route(v);
    let channel_route = v.channel_id.clone().map(Route::Channel);
    let meta = meta_line(v);
    let slot_bounds = std::rc::Rc::new(std::cell::Cell::new(None));
    let hover_key = format!("vr-{}", v.id);
    div()
        .id(SharedString::from(hover_key.clone()))
        .relative()
        .on_hover(prefetch_hover(hover_key.clone(), &v.id, slot_bounds.clone()))
        .flex()
        .gap_4()
        .w_full()
        .cursor_pointer()
        .on_aux_click(middle_click_tab(route.clone()))
        .on_click(move |_, window, cx| {
            if !crate::player::preview::consumed_click(cx) {
                navigate(route.clone(), window, cx);
            }
        })
        .child(hover_bg_plate(hover_key, thumb.clone(), px(-8.), px(16.), cx))
        .child(thumbnail_with_bounds(v, thumb, thumb_w, thumb_w * (9. / 16.), px(12.), Some(slot_bounds), cx))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().flex_1().text_lg().text_color(p.text).line_clamp(2).child(title))
                        .child(more_button(SharedString::from(format!("vrm-{}", v.id)), v, cx)),
                )
                .child(div().text_xs().text_color(p.text2).child(meta))
                .when(!v.channel_name.is_empty(), |d| {
                    let cr = channel_route.clone();
                    d.child(
                        div()
                            .id(SharedString::from(format!("vrc-{}", v.id)))
                            .flex()
                            .items_center()
                            .gap_2()
                            .py_2()
                            .text_xs()
                            .text_color(p.text2)
                            .hover(move |s| s.text_color(p.text))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                if let Some(r) = cr.clone() {
                                    navigate(r, window, cx);
                                }
                            })
                            .child(avatar(v.channel_avatar.as_deref(), px(24.), &v.channel_name, cx))
                            .child(v.channel_name.clone()),
                    )
                })
                .when_some(v.description_snippet.clone(), |d, s| {
                    d.child(div().text_xs().text_color(p.text2).line_clamp(2).child(s))
                }),
        )
        .into_any_element()
}

/// Compact row used for related videos / queues.
pub fn compact_row(v: &VideoItem, active: bool, on_click: Option<Arc<dyn Fn(&mut Window, &mut App)>>, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let (title, thumb) = branded(v, cx);
    let route = watch_route(v);
    let thumb_w = px(store::settings_snapshot().related_thumb_width);
    let thumb_h = thumb_w * (9. / 16.);
    let slot_bounds = std::rc::Rc::new(std::cell::Cell::new(None));
    let hover_key = format!("cr-{}", v.id);
    div()
        .id(SharedString::from(hover_key.clone()))
        .relative()
        .on_hover(glimpse_hover(hover_key.clone(), &v.id, slot_bounds.clone()))
        .flex()
        .gap_2()
        .w_full()
        .p_1()
        .rounded_lg()
        .cursor_pointer()
        .when(active, |d| d.bg(p.hover))
        .on_aux_click(middle_click_tab(route.clone()))
        .on_click(move |_, window, cx| {
            if crate::player::preview::consumed_click(cx) {
                return;
            }
            match &on_click {
                Some(f) => f(window, cx),
                None => navigate(route.clone(), window, cx),
            }
        })
        .child(hover_bg_plate(hover_key, thumb.clone(), px(0.), px(8.), cx))
        .child(thumbnail_with_bounds(v, thumb, thumb_w, thumb_h, px(8.), Some(slot_bounds), cx))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).line_clamp(2).child(title))
                .child(div().text_xs().text_color(p.text2).truncate().child(v.channel_name.clone()))
                .child(div().text_xs().text_color(p.text2).truncate().child(meta_line(v))),
        )
        .child(more_button(SharedString::from(format!("crm-{}", v.id)), v, cx))
        .into_any_element()
}

// ───────────────────────── channel & playlist ─────────────────────────

pub fn channel_row(c: &ChannelItem, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let id = c.id.clone();
    let subscribed = store::is_subscribed(&c.id);
    let c2 = c.clone();
    div()
        .id(SharedString::from(format!("ch-{}", c.id)))
        .flex()
        .items_center()
        .gap_4()
        .w_full()
        .py_2()
        .cursor_pointer()
        .on_click(move |_, window, cx| navigate(Route::Channel(id.clone()), window, cx))
        .child(div().w(px(360.)).flex().justify_center().child(avatar(c.avatar.as_deref(), px(136.), &c.name, cx)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap_1()
                .child(div().text_lg().text_color(p.text).child(c.name.clone()))
                .child(
                    div().text_xs().text_color(p.text2).child(
                        [c.handle.clone(), c.subscribers_text.clone(), c.video_count_text.clone()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" • "),
                    ),
                )
                .when_some(c.description.clone(), |d, s| d.child(div().text_xs().text_color(p.text2).line_clamp(2).child(s))),
        )
        .child(subscribe_button(SharedString::from(format!("chs-{}", c.id)), &c2, subscribed, cx))
        .into_any_element()
}

/// Mirrors a subscribe/unsubscribe to the signed-in YouTube account.
pub fn sync_subscription(channel_id: String, on: bool, cx: &mut App) {
    if !crate::api::auth::signed_in() || channel_id.is_empty() {
        return;
    }
    cx.spawn(async move |cx| {
        let res = blocking(move || crate::api::auth::subscribe(&channel_id, on)).await;
        if res.is_err() {
            let _ = cx.update(|cx| toast(cx, t("Couldn't update the subscription on your YouTube account")));
        }
    })
    .detach();
}

pub fn subscribe_button(id: SharedString, c: &ChannelItem, subscribed: bool, cx: &App) -> AnyElement {
    let p = pal(cx);
    let c = c.clone();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .h_9()
        .px_4()
        .rounded_full()
        .cursor_pointer()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .when(subscribed, |d| d.bg(p.chip).text_color(p.text).hover(move |s| s.bg(p.hover)).child(icon("bell").size_4()).child(t("Subscribed")))
        .when(!subscribed, |d| d.bg(p.chip_active_bg).text_color(p.chip_active_fg).child(t("Subscribe")))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            let on = !store::is_subscribed(&c.id);
            if on {
                store::subscribe(&c.id, &c.name, c.avatar.as_deref());
                toast(cx, tf!("Subscribed to {0}", c.name));
            } else {
                store::unsubscribe(&c.id);
                toast(cx, tf!("Unsubscribed from {0}", c.name));
            }
            sync_subscription(c.id.clone(), on, cx);
            cx.refresh_windows();
        })
        .into_any_element()
}

pub fn channel_card(c: &ChannelItem, width: Pixels, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let id = c.id.clone();
    let subscribed = store::is_subscribed(&c.id);
    div()
        .id(SharedString::from(format!("cc-{}", c.id)))
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .w(width)
        .py_4()
        .cursor_pointer()
        .on_click(move |_, window, cx| navigate(Route::Channel(id.clone()), window, cx))
        .child(avatar(c.avatar.as_deref(), px(104.), &c.name, cx))
        .child(div().text_base().text_color(p.text).text_center().line_clamp(1).child(c.name.clone()))
        .when_some(c.subscribers_text.clone(), |d, s| d.child(div().text_xs().text_color(p.text2).child(s)))
        .child(subscribe_button(SharedString::from(format!("ccs-{}", c.id)), c, subscribed, cx))
        .into_any_element()
}

pub fn playlist_card(pl: &PlaylistItem, width: Pixels, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let id = pl.id.clone();
    let h = width * (9. / 16.);
    div()
        .id(SharedString::from(format!("pc-{}", pl.id)))
        .flex()
        .flex_col()
        .w(width)
        .gap_3()
        .cursor_pointer()
        .on_click(move |_, window, cx| navigate(Route::Playlist(id.clone()), window, cx))
        .child(playlist_thumb(pl.thumbnail.clone(), pl.video_count_text.clone(), width, h, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(p.text).line_clamp(2).child(pl.title.clone()))
                .when_some(pl.channel_name.clone(), |d, c| d.child(div().text_sm().text_color(p.text2).truncate().child(c)))
                .child(div().text_sm().text_color(p.text2).child(t("View full playlist"))),
        )
        .into_any_element()
}

pub fn playlist_thumb(thumb: Option<String>, count: Option<String>, w: Pixels, h: Pixels, cx: &App) -> Div {
    let p = pal(cx);
    let r = px(store::settings_snapshot().thumb_radius);
    div()
        .relative()
        .w(w)
        .h(h)
        .pt_2()
        .child(
            div()
                .absolute()
                .top_0()
                .left(px(12.))
                .right(px(12.))
                .h_4()
                .rounded_t(r * 0.67)
                .bg(p.hover),
        )
        .child(
            div()
                .relative()
                .size_full()
                .rounded(r)
                .overflow_hidden()
                .bg(p.surface)
                .when_some(thumb, |d, t| d.child(img(SharedString::from(t)).size_full().rounded(r).object_fit(ObjectFit::Cover)))
                .child(
                    div()
                        .absolute()
                        .bottom_1()
                        .right_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_1()
                        .rounded_sm()
                        .bg(hsla(0., 0., 0., 0.8))
                        .text_xs()
                        .text_color(white())
                        .child(icon("list-video").size_3().text_color(white()))
                        .child(count.unwrap_or_default()),
                ),
        )
}

pub fn playlist_row(pl: &PlaylistItem, cx: &mut App) -> AnyElement {
    let p = pal(cx);
    let id = pl.id.clone();
    div()
        .id(SharedString::from(format!("pr-{}", pl.id)))
        .flex()
        .gap_4()
        .w_full()
        .cursor_pointer()
        .on_click(move |_, window, cx| navigate(Route::Playlist(id.clone()), window, cx))
        .child(playlist_thumb(pl.thumbnail.clone(), pl.video_count_text.clone(), px(360.), px(202.), cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .child(div().text_lg().text_color(p.text).line_clamp(2).child(pl.title.clone()))
                .when_some(pl.channel_name.clone(), |d, c| d.child(div().text_xs().text_color(p.text2).child(c)))
                .child(div().text_xs().text_color(p.text2).font_weight(FontWeight::MEDIUM).child(t("View full playlist"))),
        )
        .into_any_element()
}

/// Renders any item in a grid.
pub fn grid_item(item: &Item, width: Pixels, show_channel: bool, cx: &mut App) -> AnyElement {
    match item {
        Item::Video(v) => video_card(v, width, show_channel, cx),
        Item::Channel(c) => channel_card(c, width, cx),
        Item::Playlist(p) => playlist_card(p, width, cx),
    }
}

/// Grid layout: computes columns from available width.
pub fn grid(items: Vec<AnyElement>, _avail: Pixels) -> Div {
    div().flex().flex_wrap().gap_x_4().gap_y(grid_row_gap()).w_full().children(items)
}

pub fn grid_row_gap() -> Pixels {
    px(match store::settings_snapshot().density.as_str() {
        "compact" => 20.,
        "comfortable" => 48.,
        _ => 36.,
    })
}

pub fn grid_columns(avail: Pixels, min_card: f32, gap: Pixels) -> (usize, Pixels) {
    let cfg = store::settings_snapshot().grid_columns as usize;
    let gap: f32 = gap.into();
    let w: f32 = avail.into();
    let cols = if cfg > 0 { cfg } else { (((w + gap) / (min_card + gap)).floor() as usize).clamp(1, 6) };
    let card = (w - gap * (cols as f32 - 1.0)) / cols as f32;
    (cols, px(card.floor() - 1.0))
}
