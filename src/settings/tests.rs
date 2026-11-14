use super::*;

/// Every row showing: Custom shows every AI field.
fn sample() -> Values {
    Values {
        background: Some(0),
        theme: "Temino".into(),
        themes: 12,
        palettes: Vec::new(),
        palette: 0,
        editor: None,
        mode: Mode::Dark,
        keys: Vec::new(),
        recording: None,
        ligatures: true,
        font: "JetBrains Mono".into(),
        fonts: Vec::new(),
        font_index: 0,
        quick_terminal: Some(None),
        opacity: 0,
        blur: false,
        shell: Some(0),
        installed: [true, true, false],
        fetch: 1,
        fetches_installed: [true, false, true],
        tabs_top: false,
        extended: false,
        notifications: true,
        resume_programs: false,
        resume_servers: false,
        zoom: 100.0,
        points: 15.4,
        line_spacing: 1,
        cursor: 0,
        blink: true,
        updates: true,
        analytics: true,
        see_through: true,
        remote: None,
        remote_account: String::new(),
        remote_url: String::new(),
        remote_phones: Vec::new(),
        provider: PROVIDERS.len() - 1,
        fields: Default::default(),
        editing: None,
        previewing: false,
    }
}

#[test]
fn every_control_can_be_clicked() {
    let mut scene = Scene::new();
    let mut text = TextSystem::new(14.0);
    let theme = Theme::dark();
    let values = sample();
    let mut page = Page::default();
    let rect = Rect::new(200.0, 0.0, 900.0, 8000.0);
    // Every sub tab in turn: the bar first, then its controls.
    let mut actions = Vec::new();
    for section in Section::shown(&values) {
        page.show(section);
        scene.reset(1100.0, 8000.0, 1.0, theme.background);
        page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
        let bar: Vec<Action> = Section::shown(&values).into_iter().map(Action::Section).collect();
        let drawn: Vec<Action> = page.hits.iter().map(|&(_, a)| a).collect();
        assert_eq!(drawn[..bar.len()], bar[..]);
        actions.extend_from_slice(&drawn[bar.len()..]);
        // A click lands on what's under it, and nothing overlaps.
        for (rect, action) in &page.hits {
            let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            assert_eq!(page.action_at(x, y), Some(*action));
        }
    }
    let mut expected = vec![Action::Theme(-1), Action::List(List::Themes), Action::Theme(1)];
    expected.extend(Mode::ALL.map(Action::Mode));
    expected.push(Action::OpenThemes);
    expected.extend((0..6).map(Action::Background));
    expected.extend((0..OPACITIES.len()).map(Action::Opacity));
    expected.extend([
        Action::Blur(true),
        Action::Font(-1),
        Action::List(List::Fonts),
        Action::Font(1),
        Action::Zoom(-1),
        Action::Zoom(0),
        Action::Zoom(1),
    ]);
    expected.extend((0..LINE_SPACINGS.len()).map(Action::LineSpacing));
    expected.push(Action::Ligatures(false));
    expected.extend((0..Shape::ALL.len()).map(Action::Cursor));
    expected.push(Action::Blink(false));
    expected.extend((0..KINDS.len()).map(Action::Shell));
    expected.extend((0..=FETCHES.len()).map(Action::Fetch));
    expected.push(Action::FetchConfig);
    expected.extend([
        Action::TabsTop(false),
        Action::TabsTop(true),
        Action::Extended(true),
        Action::Notifications(false),
        Action::ResumePrograms(true),
        Action::ResumeServers(true),
    ]);
    expected.extend((0..PROVIDERS.len()).map(Action::Provider));
    expected.extend(Field::ALL.map(Action::Edit));
    // Keys: nothing changed, so no Reset All; a shortcut per action.
    expected.extend([Action::EditKeys, Action::QuickTerminal(true)]);
    for (_, group) in crate::bindings::GROUPS {
        expected.extend(group.iter().filter(|a| keys::shown(a)).map(|&a| Action::RecordKey(a)));
    }
    expected.extend([Action::Updates(false), Action::Analytics(false), Action::CheckForUpdates]);
    assert_eq!(actions, expected);
}

#[test]
fn remote_control_has_a_tab_only_with_a_relay() {
    let mut values = sample();
    values.remote = None;
    assert!(!Section::shown(&values).contains(&Section::Remote));
    values.remote = Some(false);
    assert_eq!(Section::shown(&values), Section::ALL);
    // The account menu's Remote Control opens its tab from the top.
    let mut page = Page { scroll: 200.0, to_remote: true, ..Page::default() };
    let actions = draw_once(&mut page, &values);
    assert_eq!(page.section, Section::Remote);
    assert_eq!(page.scroll, 0.0);
    assert!(actions.contains(&Action::Remote(true)));
    // Without the relay again, the page falls back to its first tab.
    values.remote = None;
    draw_once(&mut page, &values);
    assert_eq!(page.section, Section::Appearance);
}

#[test]
fn a_tab_opens_at_its_top_with_no_list_open() {
    let mut page = Page { scroll: 300.0, ..Page::default() };
    page.toggle(List::Fonts);
    page.show(Section::Keys);
    assert_eq!((page.section, page.scroll, page.open), (Section::Keys, 0.0, None));
    // Its own tab again changes nothing.
    page.scroll = 120.0;
    page.show(Section::Keys);
    assert_eq!(page.scroll, 120.0);
}

#[test]
fn the_theme_editor_offers_every_color_of_a_theme_of_your_own() {
    let mut scene = Scene::new();
    let mut text = TextSystem::new(14.0);
    let theme = Theme::dark();
    let mut palette = crate::theme::library("/nonexistent")[1].copy_as("Mine");
    palette.file = Some("/nonexistent/Mine".into());
    let edit = ThemeEdit { palette, slot: Slot::Ansi(1), confirm_delete: false };
    let values = Values { editor: Some(edit), ..sample() };
    let mut page = Page::default();
    let rect = Rect::new(200.0, 0.0, 900.0, 3000.0);
    scene.reset(1100.0, 3000.0, 1.0, theme.background);
    page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
    let actions: Vec<Action> = page.hits.iter().map(|&(_, a)| a).collect();
    // The editor: name, 5 + 8 + 8 colors, hex, three nudges, delete.
    let start = actions.iter().position(|&a| a == Action::Edit(Field::ThemeName)).unwrap();
    let mut expected = vec![Action::Edit(Field::ThemeName)];
    expected.extend(Slot::BASE.map(Action::Slot));
    expected.extend((0..16).map(|i| Action::Slot(Slot::Ansi(i))));
    expected.push(Action::Edit(Field::ThemeHex));
    for channel in [Channel::Lightness, Channel::Saturation, Channel::Hue] {
        expected.extend((-1..=1).map(|step| Action::Nudge(channel, step)));
    }
    // Done, Duplicate and Delete, right to left.
    expected.extend([Action::ThemeEditor(false), Action::DuplicateTheme, Action::DeleteTheme]);
    assert_eq!(actions[start..start + expected.len()], expected[..]);
    for (rect, action) in &page.hits {
        let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        assert_eq!(page.action_at(x, y), Some(*action), "{action:?}");
    }
    // Closed, the editor's rows are gone.
    page.draw(&mut scene, &mut text, &theme, rect, &sample(), (0.0, 0.0));
    let actions: Vec<Action> = page.hits.iter().map(|&(_, a)| a).collect();
    assert!(!actions.iter().any(|a| matches!(a, Action::Slot(_) | Action::DeleteTheme)));
}

#[test]
fn the_page_scrolls_only_as_far_as_it_goes() {
    let mut page = Page { content_height: 900.0, view_height: 600.0, ..Page::default() };
    page.wheel(100.0, 0.0, 0.0);
    assert_eq!(page.scroll, 300.0);
    page.wheel(-100.0, 0.0, 0.0);
    assert_eq!(page.scroll, 0.0);
}

#[test]
fn drawing_clamps_a_scroll_the_content_no_longer_reaches() {
    let mut scene = Scene::new();
    let mut text = TextSystem::new(14.0);
    let theme = Theme::dark();
    let values = sample();
    // Scrolled to the end of 900px of content in a 600px view, then
    // the window grows past the whole content: nothing to scroll.
    let mut page = Page { content_height: 900.0, view_height: 600.0, ..Page::default() };
    page.scroll = 300.0;
    let rect = Rect::new(0.0, 0.0, 900.0, 2000.0);
    scene.reset(900.0, 2000.0, 1.0, theme.background);
    page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
    assert_eq!(page.scroll, 0.0);
    // Content still taller than the view: the scroll is capped at
    // the new end, not zeroed.
    page.content_height = 900.0;
    page.scroll = 500.0;
    let rect = Rect::new(0.0, 0.0, 900.0, 600.0);
    scene.reset(900.0, 600.0, 1.0, theme.background);
    page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
    assert_eq!(page.scroll, 300.0);
}

#[test]
fn keys_show_as_dots_and_nothing_else_does() {
    let secret: Vec<_> = Field::ALL.iter().filter(|f| f.secret()).copied().collect();
    assert_eq!(secret, [Field::LlmKey, Field::TypesafeKey]);
    // Each field reads its own slot in `Values::fields`.
    for (i, field) in Field::ALL.into_iter().enumerate() {
        assert_eq!(field.index(), Some(i));
    }
}

#[test]
fn a_page_that_fits_never_scrolls() {
    let mut page = Page { content_height: 400.0, view_height: 600.0, ..Page::default() };
    page.wheel(5.0, 0.0, 0.0);
    assert_eq!(page.scroll, 0.0);
    page.wheel(-5.0, 0.0, 0.0);
    assert_eq!(page.scroll, 0.0);
}

#[test]
fn the_wheel_scrolls_the_theme_list_only_while_over_it() {
    let list = Rect::new(100.0, 100.0, 200.0, 300.0);
    let mut page = Page { content_height: 2000.0, view_height: 600.0, list, ..Page::default() };
    page.toggle(List::Themes);
    assert_eq!(page.open, Some(List::Themes));
    page.list_scroll = 0.0;
    page.wheel(2.0, 150.0, 200.0);
    assert_eq!((page.list_scroll, page.scroll), (2.0 * LIST_ROW, 0.0));
    page.wheel(1.0, 50.0, 200.0);
    assert_eq!((page.list_scroll, page.scroll), (2.0 * LIST_ROW, WHEEL_STEP));
    // Closed, the list's old place scrolls the page.
    page.toggle(List::Themes);
    assert_eq!(page.open, None);
    page.wheel(1.0, 150.0, 200.0);
    assert_eq!(page.scroll, 2.0 * WHEEL_STEP);
}

#[test]
fn a_click_goes_to_what_was_drawn_last_under_it() {
    let row = Rect::new(0.0, 0.0, 400.0, 60.0);
    let list = Rect::new(100.0, 30.0, 200.0, 200.0);
    let page = Page { hits: vec![(row, Action::Ligatures(true)), (list, Action::PickTheme(3))], ..Page::default() };
    assert_eq!(page.action_at(10.0, 10.0), Some(Action::Ligatures(true)));
    assert_eq!(page.action_at(150.0, 40.0), Some(Action::PickTheme(3)));
    assert_eq!(page.action_at(150.0, 250.0), None);
    // Rectangles take their top-left edge, not their bottom-right one.
    assert_eq!(page.action_at(400.0, 10.0), None);
}

/// Draws the page's sub tab showing once and returns every click it offers.
fn draw_once(page: &mut Page, values: &Values) -> Vec<Action> {
    let mut scene = Scene::new();
    let mut text = TextSystem::new(14.0);
    let theme = Theme::dark();
    let rect = Rect::new(200.0, 0.0, 900.0, 8000.0);
    scene.reset(1100.0, 8000.0, 1.0, theme.background);
    page.draw(&mut scene, &mut text, &theme, rect, values, (0.0, 0.0));
    page.hits.iter().map(|&(_, a)| a).collect()
}

/// Draws every sub tab with `values` and returns every click they offer,
/// then goes back to the tab it was on.
fn actions_of(page: &mut Page, values: &Values) -> Vec<Action> {
    let was = page.section;
    let mut actions = Vec::new();
    for section in Section::shown(values) {
        page.section = section;
        actions.extend(draw_once(page, values));
    }
    page.section = was;
    draw_once(page, values);
    actions
}

#[test]
fn the_theme_list_ends_with_customize() {
    let palettes = crate::theme::library("/nonexistent");
    let values = Values { palettes: palettes.clone(), palette: 2, ..sample() };
    let mut page = Page { open: Some(List::Themes), ..Page::default() };
    let actions = draw_once(&mut page, &values);
    assert_eq!(page.open, Some(List::Themes));
    let at = actions.iter().position(|&a| a == Action::PickTheme(0)).unwrap();
    // The list's padding first (the one in use), then every theme, then Customize….
    assert_eq!(actions[at - 1], Action::PickTheme(2));
    let picks: Vec<Action> = (0..palettes.len()).map(Action::PickTheme).collect();
    assert_eq!(actions[at..at + palettes.len()], picks[..]);
    assert_eq!(actions[at + palettes.len()], Action::Customize);
}

#[test]
fn the_font_name_drops_a_list_of_every_font() {
    let fonts: Vec<String> = ["JetBrains Mono (built in)", "Menlo", "Monaco"].map(String::from).to_vec();
    let values = Values { fonts, font_index: 1, ..sample() };
    let mut page = Page { open: Some(List::Fonts), ..Page::default() };
    let actions = draw_once(&mut page, &values);
    let at = actions.iter().position(|&a| a == Action::PickFont(0)).unwrap();
    assert_eq!(actions[at - 1..at + 3], [1, 0, 1, 2].map(Action::PickFont));
    assert!(!actions.contains(&Action::Customize));
}

#[test]
fn a_changed_shortcut_offers_reset_and_waiting_shows_one_cap() {
    let (keys, _) = crate::bindings::parse("ctrl+t = new_tab\ncmd+j = text:ls\\r\n");
    let values = Values { keys, recording: Some("copy"), ..sample() };
    let actions = actions_of(&mut Page::default(), &values);
    // New tab: Reset, then its two keys (right to left).
    let at = actions.iter().position(|&a| a == Action::ResetKey("new_tab")).unwrap();
    assert_eq!(actions[at - 2..at], [Action::RecordKey("new_tab"); 2]);
    assert!(actions.contains(&Action::ResetKeys));
    // Copy waits for keys: one cap, no Reset.
    assert_eq!(actions.iter().filter(|&&a| a == Action::RecordKey("copy")).count(), 1);
    assert!(!actions.contains(&Action::ResetKey("copy")));
    // The key that types text can be removed.
    let combo = crate::bindings::parse_combo("cmd+j").unwrap();
    assert!(actions.contains(&Action::RemoveTyped(combo)));
}

/// Where the font name (the font list's anchor) is drawn.
fn font_name_at(text: &mut TextSystem) -> Rect {
    let mut scene = Scene::new();
    let theme = Theme::dark();
    let mut page = Page::default();
    scene.reset(1100.0, 8000.0, 1.0, theme.background);
    page.draw(&mut scene, text, &theme, Rect::new(200.0, 0.0, 900.0, 8000.0), &sample(), (0.0, 0.0));
    page.hits.iter().find(|&&(_, a)| a == Action::List(List::Fonts)).map(|&(rect, _)| rect).unwrap()
}

#[test]
fn the_sample_terminal_sits_above_font_and_follows_line_spacing() {
    let mut text = TextSystem::new(14.0);
    text.set_leading(LINE_SPACINGS[0].1);
    let tight = font_name_at(&mut text);
    let before = preview::sample_height(&text);
    text.set_leading(LINE_SPACINGS[3].1);
    let grown = preview::sample_height(&text) - before;
    assert!(grown > 0.0);
    // Only the sample, above Font, grows with the line spacing.
    let airy = font_name_at(&mut text);
    assert!((airy.y - tight.y - grown).abs() < 0.01, "{} vs {}", airy.y - tight.y, grown);
}

#[test]
fn hovering_a_visual_choice_previews_it() {
    let lists = [None, Some(List::Themes), Some(List::Fonts)];
    // A list's names only while that list is open.
    assert_eq!(Preview::of(Action::PickFont(2), Some(List::Fonts)), Some(Preview::Font(2)));
    assert_eq!(Preview::of(Action::PickFont(2), Some(List::Themes)), None);
    assert_eq!(Preview::of(Action::PickTheme(1), Some(List::Themes)), Some(Preview::Theme(1)));
    assert_eq!(Preview::of(Action::PickTheme(1), None), None);
    for open in lists {
        assert_eq!(Preview::of(Action::Background(3), open), Some(Preview::Background(3)));
        assert_eq!(Preview::of(Action::LineSpacing(2), open), Some(Preview::LineSpacing(2)));
        assert_eq!(Preview::of(Action::Cursor(1), open), Some(Preview::Cursor(1)));
        assert_eq!(Preview::of(Action::Opacity(4), open), Some(Preview::Opacity(4)));
        // Not a look: nothing to preview.
        assert_eq!(Preview::of(Action::Ligatures(true), open), None);
        assert_eq!(Preview::of(Action::Zoom(1), open), None);
    }
    assert!(Preview::Font(1).same_setting(Preview::Font(4)));
    assert!(!Preview::Font(1).same_setting(Preview::LineSpacing(1)));
}

#[test]
fn a_previewed_cursor_shape_is_drawn_but_not_picked() {
    use crate::cursor::{self, Shape};
    let picked = cursor::picked();
    let other = if picked == Shape::Underline { Shape::Block } else { Shape::Underline };
    cursor::preview(Some(other));
    assert_eq!((cursor::shape(), cursor::picked()), (other, picked));
    cursor::preview(None);
    assert_eq!(cursor::shape(), picked);
}

/// Hovering a font or a line spacing previews it in the sample, but the
/// sample keeps its size: the rows under the pointer don't move.
#[test]
fn a_preview_does_not_move_the_page() {
    let mut scene = Scene::new();
    let mut text = TextSystem::new(14.0);
    let theme = Theme::dark();
    let mut page = Page::default();
    let rect = Rect::new(200.0, 0.0, 900.0, 8000.0);
    let mut layout = |page: &mut Page, text: &mut TextSystem, previewing: bool| {
        scene.reset(1100.0, 8000.0, 1.0, theme.background);
        let values = Values { previewing, ..sample() };
        page.draw(&mut scene, text, &theme, rect, &values, (0.0, 0.0));
        page.hits.iter().map(|&(r, _)| (r.x, r.y)).collect::<Vec<_>>()
    };
    let before = layout(&mut page, &mut text, false);
    let leading = text.leading();
    text.set_leading(leading + 6.0);
    assert_eq!(layout(&mut page, &mut text, true), before, "a preview moved the page");
    // Picked for real, the sample grows to fit.
    assert_ne!(layout(&mut page, &mut text, false), before);
}

/// Picking another font keeps the sample's size: its box follows the
/// font size and line spacing, not the font's own line height.
#[test]
fn another_font_does_not_resize_the_sample() {
    let mut text = TextSystem::new(14.0);
    let before = preview::sample_height(&text);
    let pitch = text.cell.height;
    // Any installed monospaced family with a different line height.
    let other = crate::fonts::families().into_iter().find(|family| {
        let mut probe = TextSystem::new(14.0);
        probe.set_mono(Some(family)).is_ok() && probe.cell.height != pitch
    });
    let Some(family) = other else { return };
    text.set_mono(Some(&family)).unwrap();
    assert_ne!(text.cell.height, pitch);
    assert_eq!(preview::sample_height(&text), before, "{family} resized the sample");
}
